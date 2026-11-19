declare function require(name: string): any;
import { createElement } from "react";
import { renderToStaticMarkup } from "react-dom/server";
import { parseStagedRow, type StagedWrite } from "../src/features/decisions/stagedWrite";
import type { DecisionsState } from "../src/features/decisions/useDecisions";
import { TurnSources, TurnStagedCards } from "../src/features/home/TurnRichCards";
import type { ThreadEntry } from "../src/features/home/thread";
import { turnSources } from "../src/features/home/turnCards";
import { StagedSourceContext, type StagedSource } from "../src/ui/StagedChip";
import { NavigationProvider, type NavigationAdapter } from "../screen-ui/src/navigation";

/**
 * CAD-2441: the rendered Sources row and staged action card — the cap, the
 * card's status following the decisions list, and no approve control.
 */

function check(ok: boolean, what: string): void {
  if (!ok) throw new Error(`turnRichCards: ${what}`);
}
const count = (html: string, needle: string): number => html.split(needle).length - 1;
// The board's router gives in-app links their URL; a test host does the same.
const nav: NavigationAdapter = { navigate: () => {}, href: (href) => href, modifiedClicks: "browser" };
const sourcesHtml = (props: Parameters<typeof TurnSources>[0]): string =>
  renderToStaticMarkup(createElement(NavigationProvider, { adapter: nav, children: createElement(TurnSources, props) }));

let seq = 0;
const call = (text: string): ThreadEntry => ({ seq: ++seq, role: "agent", kind: "tool_call", text, message: "m1", payload: null });
const result = (text: string): ThreadEntry => ({ seq: ++seq, role: "agent", kind: "tool_result", text, message: "m1", payload: null });

// The row: 8 chips, then "+N more"; nothing when the turn read nothing.
{
  const entries = Array.from({ length: 11 }, (_, i) => call(`wiki_read {"path":"notes/page-${i}"}`));
  const sources = turnSources(entries);
  check(sources.length === 11, "eleven distinct sources");
  const html = sourcesHtml({ sources, artifacts: [] });
  check(count(html, 'class="turnsource"') === 8, `8 chips shown, got ${count(html, 'class="turnsource"')}`);
  check(html.includes("+3"), "the rest are +3 more");
  check(html.includes('aria-label="Sources · 11"'), "the row names the full count");
  check(html.includes('href="/wiki/notes/page-0"'), "a wiki chip links to its page");
  check(!html.includes("page-8"), "the ninth source is behind +more");
  check(sourcesHtml({ sources: [], artifacts: [] }) === "", "no row when nothing was read");
}

// An app record chip names and opens the record.
{
  const [source] = turnSources([call('app_call {"app":"crm","operation":"records.show","args":"{\\"record_id\\":\\"cust-42\\"}"}')]);
  const html = sourcesHtml({ sources: [source!], artifacts: [] });
  check(html.includes("crm · cust-42"), "the chip names the app and record");
  check(html.includes('href="/apps/crm?record=cust-42"'), `the chip opens the record: ${html}`);
  const [odd] = turnSources([call('app_call {"app":"crm","operation":"records.show","args":{"record_id":"Not An Id!"}}')]);
  const oddHtml = sourcesHtml({ sources: [odd!], artifacts: [] });
  check(!oddHtml.includes("href="), "a record id the app screen would refuse is not a link");
}

// CAD-2450: an artifact chip is named by the artifact's title when the turn holds it.
{
  const id = "art-4cff6ebbaae44734856e9acb4549e320";
  const sources = turnSources([call(`artifact_read {"id":"${id}"}`)]);
  const held: ThreadEntry = { seq: ++seq, role: "agent", kind: "artifact", text: "", message: "m1", payload: { id, version: 1, kind: "md", title: "Quote draft", bytes: 10, digest: "d" } };
  const titled = sourcesHtml({ sources, artifacts: [held] });
  check(titled.includes("<bdi>Quote draft</bdi>") && titled.includes(`title="${id}"`), `a held artifact is named by its title: ${titled}`);
  check(/<button[^>]*data-source-kind="artifact"/.test(titled), "a held artifact opens");
  const bare = sourcesHtml({ sources, artifacts: [] });
  check(bare.includes("<bdi>art-4cff6ebb…</bdi>") && bare.includes(`title="${id}"`), "an artifact from another turn is a short id with the full id in the tooltip");
  check(!/<(a|button)[^>]*data-source-kind="artifact"/.test(bare), "with no opener it is plain text");
}

// Trust boundary: a web chip is an anchor only through SafeLink's policy.
{
  const web = (url: string) => sourcesHtml({ sources: [{ kind: "web", key: `web:${url}`, label: "x", target: url }], artifacts: [] });
  const ok = web("https://example.com/a?b=1");
  check(/<a [^>]*href="https:\/\/example\.com\/a\?b=1"[^>]*target="_blank"[^>]*rel="noreferrer"/.test(ok), `an https source is an external anchor: ${ok}`);
  for (const url of ["http://localhost:3010/api", "http://127.0.0.1:9451/", "http://[::1]/x"]) {
    const html = web(url);
    check(!/<a [^>]*href=/.test(html) && html.includes("text-warn"), `a link to this machine is a warning, never an anchor: ${url}`);
  }
  for (const url of ["javascript:alert(1)", "data:text/html,x", "/wiki/x"]) {
    check(!/<a [^>]*href=/.test(web(url)), `a non-http source is never an anchor: ${url}`);
  }
  // Derivation already drops non-http schemes before render.
  check(turnSources([call('web_read {"url":"javascript:alert(1)"}'), call('web_read {"url":"file:///etc/passwd"}')]).length === 0, "non-http reads are not sources");
}

// The staged card reads the decisions list the surface holds.
const ID = "7d2e4c1a9b3f40e8a6c5d2b1f0e9a873";
const row = (status: string, result: Record<string, unknown> | undefined = undefined): StagedWrite =>
  parseStagedRow({
    id: ID,
    tool: "app_call",
    status,
    createdAt: 1_790_000_000_000,
    decidedAt: status === "pending" ? undefined : 1_790_000_100_000,
    ...(result ? { result } : {}),
    app: "crm",
    title: "Add customer Northwind",
    changes: [
      { field: "name", label: "Name", kind: "added", after: "Northwind" },
      { field: "email", label: "Email", kind: "added", after: "" },
      { field: "fax", label: "Fax", kind: "removed", before: "555-0100" },
    ],
    changes_complete: true,
    input: { app: "crm", subject: "Add customer Northwind", operation: "records.create", args: '{"name":"Northwind","email":""}' },
  })!;

function sourceWith(rows: StagedWrite[]): StagedSource {
  const state = { phase: "ready", rows, pending: rows.filter((r) => r.status === "pending"), decided: [], attempts: new Map(), notice: null } as unknown as DecisionsState;
  return { subscribe: () => () => {}, currentDecisions: () => state, requestReview: () => {} };
}
const renderCards = (rows: StagedWrite[]): string =>
  renderToStaticMarkup(
    createElement(
      StagedSourceContext.Provider,
      { value: sourceWith(rows) },
      createElement(TurnStagedCards, { toolEntries: [result(`{"staged":true,"action_id":"${ID}"}`)], density: "full" }),
    ),
  );

{
  const pending = renderCards([row("pending")]);
  check(pending.includes(`data-staged-id="${ID}"`), "the staged write renders as a card");
  check(pending.includes('data-outcome="pending"') && pending.includes(">Waiting<"), "pending reads Waiting");
  check(pending.includes("data-staged-review") && pending.includes("data-staged-later"), "pending offers Review and Not now");
  check(pending.includes("Not provided"), "the empty email is flagged");
  const removedField = pending.slice(pending.indexOf("data-removed"));
  check(removedField.includes("<del><bdi>555-0100</bdi></del>"), "a removed field shows what it removes, struck through");
  check(!removedField.slice(0, removedField.indexOf("</div>")).includes("Not provided"), "a removed field is not flagged as missing");
  check(pending.includes(">Add customer Northwind<"), "the card names the write");

  const applied = renderCards([row("approved", { ok: true })]);
  check(applied.includes('data-outcome="applied"') && applied.includes(">Applied<"), `the status follows the list to Applied: ${applied}`);
  check(!applied.includes("data-staged-review"), "a decided write offers no Review");

  const denied = renderCards([row("denied")]);
  check(denied.includes('data-outcome="denied"') && denied.includes(">Denied<"), "the status follows the list to Denied");

  check(renderCards([]) === "", "a write the list does not hold shows no card");

  // CAD-1109 D3: approving and denying stay on the board's review card.
  for (const html of [pending, applied, denied]) {
    check(!/>\s*(Approve|Deny|Reject|Accept)\s*</i.test(html), "no approve or deny control in chat");
    check(!/data-(decide|approve|deny)/.test(html), "no decision hook in chat");
  }
}

check(
  renderToStaticMarkup(createElement(TurnStagedCards, { toolEntries: [result(ID)], density: "full" })) === "",
  "without a decisions source, no card",
);

// CAD-2463: the connected-effect staged card (AOS-647 email preview fixture).
{
  const input = JSON.parse(require("fs").readFileSync(require("path").join(require("process").cwd(), "tests", "fixtures", "chat-cards", "connected-effect-email.json"), "utf8")) as Record<string, unknown>;
  const preview = input.preview as { fields: { role: string; label: string; value: string; untrusted: boolean }[] };
  const longBody = "Line one.\n".repeat(30) + "<b>not markup</b>";
  const fields = preview.fields.map((f) => (f.role === "to" ? { ...f, value: "mei@harbourfans.example" } : f.role === "body" ? { ...f, value: longBody } : f));
  const emailRow = parseStagedRow({ id: ID, tool: "connected_effect", status: "pending", createdAt: 1, input: { ...input, preview: { ...preview, fields } } })!;
  const html = renderToStaticMarkup(
    createElement(StagedSourceContext.Provider, { value: sourceWith([emailRow]) }, createElement(TurnStagedCards, { toolEntries: [result(`{"staged":true,"actionId":"${ID}"}`)], density: "full" })),
  );
  check(html.includes("Synthetic Mail · ") && html.includes('class="turncard-from">from <bdi>ops@example.test</bdi>'), `the header names the toolkit and the sending account: ${html}`);
  check(html.includes('data-message-kind="email"') && html.includes("data-clamped"), "the email body is clamped");
  check(html.includes("Show full message") && html.includes('aria-expanded="false"'), "the body offers Show full message");
  check(html.includes("&lt;b&gt;not markup&lt;/b&gt;") && !html.includes("<b>not markup</b>"), "the body is plain text");
  check(html.includes("Line one.") && (html.match(/Line one\./g) ?? []).length === 30, "the card carries the whole body, not the 280-character cut");
  check(html.includes('data-outside-domain="1"') && html.includes("1 recipient is outside your company domain"), "one outside recipient is flagged");
  check(!/<dt><bdi>Body<\/bdi><\/dt>/.test(html), "the body is not repeated as a field");
  check(!/>\s*(Approve|Deny)\s*</.test(html), "still no approve or deny in chat");

  const sent = parseStagedRow({ id: ID, tool: "connected_effect", status: "approved", createdAt: 1, decidedAt: 2, result: { ok: true }, input })!;
  const sentHtml = renderToStaticMarkup(
    createElement(StagedSourceContext.Provider, { value: sourceWith([sent]) }, createElement(TurnStagedCards, { toolEntries: [result(`{"staged":true,"actionId":"${ID}"}`)], density: "full" })),
  );
  check(sentHtml.includes(">Sent<") && !sentHtml.includes("data-outside-domain"), `an applied send reads Sent: ${sentHtml}`);
}

console.log("turnRichCards: ok");
