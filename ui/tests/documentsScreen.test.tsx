/**
 * CAD-2286: the Documents screen, mounted in happy-dom against a fake board.
 *
 * It covers each state (unavailable, checking, probe error, loading, list
 * error, empty, the list), the Active/Archived tabs and the stage chips in the
 * URL, opening a file in the canvas drawer, and the review actions: Approve,
 * Archive and Restore appear only where the access rules allow them, sit behind
 * a confirmation, and turn each refusal into translated copy. A write that
 * rejects with an AbortError may have been applied, so the list is read again.
 */
declare function require(name: string): any;
export {};

function ok(value: unknown, why: string): asserts value {
  if (!value) throw new Error(why);
}
function equal(actual: unknown, expected: unknown, why: string): void {
  const a = JSON.stringify(actual);
  const e = JSON.stringify(expected);
  if (a !== e) throw new Error(`${why}: expected ${e}, got ${a}`);
}

async function main() {
  const { Window } = require("happy-dom");
  const win = new Window({ url: "http://localhost/documents" });
  for (const name of [
    "window", "document", "Node", "Element", "HTMLElement", "HTMLInputElement", "HTMLButtonElement", "SVGElement",
    "navigator", "MutationObserver", "Event", "MouseEvent", "KeyboardEvent", "location", "history", "localStorage",
    "sessionStorage",
  ])
    Object.defineProperty(globalThis, name, { value: name === "window" ? win : win[name], configurable: true, writable: true });
  for (const name of ["addEventListener", "removeEventListener", "dispatchEvent"])
    Object.defineProperty(globalThis, name, { value: win[name].bind(win), configurable: true });
  Object.defineProperty(globalThis, "IS_REACT_ACT_ENVIRONMENT", { value: true });

  const loader = require("module");
  const originalRequire = loader.prototype.require;
  loader.prototype.require = function (this: unknown, id: string) {
    if (id.endsWith(".css")) return {};
    if (id === "@hugeicons/core-free-icons") return new Proxy({}, { get: () => ({}) });
    if (id === "@hugeicons/react") return { HugeiconsIcon: () => null };
    if (id === "react-markdown") return { __esModule: true, default: () => null };
    if (id === "remark-gfm") return { __esModule: true, default: () => undefined };
    return originalRequire.apply(this, arguments);
  };

  const React = require("react") as typeof import("react");
  const { createRoot } = require("react-dom/client") as typeof import("react-dom/client");
  const { LocaleProvider, translateUiText } = require("../src/lib/locale") as typeof import("../src/lib/locale");
  const screenModule = require("../src/features/documents/DocumentsScreen") as typeof import("../src/features/documents/DocumentsScreen");
  const DocumentsScreen = screenModule.default;
  const { DOCUMENTS_CONVERSATION } = screenModule;
  const { DOCUMENT_CAPABILITY } = require("../src/features/documents/documentsClient") as typeof import("../src/features/documents/documentsClient");
  const { documentsViewer } = require("../src/features/documents/documentsAccess") as typeof import("../src/features/documents/documentsAccess");
  const { documentsProbe } = require("../src/features/documents/documentsProbe") as typeof import("../src/features/documents/documentsProbe");
  const { canvasSnapshot, resetCanvas, selectVersion } = require("../src/features/canvas/canvasStore") as typeof import("../src/features/canvas/canvasStore");
  const CanvasDrawer = (require("../src/features/canvas/CanvasDrawer") as typeof import("../src/features/canvas/CanvasDrawer")).default;
  const { allowPrivateReads, pausePrivateReads, resetPrivateSession } = require("../src/lib/privateSession") as typeof import("../src/lib/privateSession");
  const { goTo, locationHref, readLocation } = require("../src/lib/router") as typeof import("../src/lib/router");
  const { NavigationProvider } = require("../screen-ui/src/navigation") as typeof import("../screen-ui/src/navigation");
  const { hostNavigation } = require("../src/ui/hostNavigation") as typeof import("../src/ui/hostNavigation");
  type Route = import("../src/lib/router").Route;
  type Viewer = import("../src/features/documents/documentsAccess").DocumentsViewer;

  // ---- the fake board ----

  type Reply = Response | Promise<Response>;
  const json = (value: unknown, status = 200) =>
    new Response(JSON.stringify(value), { status, headers: { "Content-Type": "application/json" } });
  const refusal = (status: number, code: string) => json({ ok: false, error: { code, message: `server says ${code}` } }, status);

  type Call = { method: string; path: string; body: unknown };
  let calls: Call[] = [];
  let probeReply: () => Reply = () => json(probeBody());
  let listReply: (archived: boolean) => Reply = (archived) => json({ documents: archived ? ARCHIVED : ACTIVE });
  let writeReply: (path: string, body: unknown) => Reply = () => refusal(500, "internal");
  globalThis.fetch = (async (input: unknown, init?: RequestInit) => {
    const path = String(input);
    const method = init?.method ?? "GET";
    const body = typeof init?.body === "string" ? JSON.parse(init.body) : null;
    calls.push({ method, path, body });
    if (path === "/api/document-types") return probeReply();
    if (path.startsWith("/api/documents?archived=")) return listReply(path.endsWith("archived=1"));
    if (method === "POST" && path.startsWith("/api/documents/")) return writeReply(path, body);
    return refusal(404, "not_found");
  }) as typeof fetch;

  const BASELINE = {
    stages: ["draft", "in_review", "approved", "delivered", "archived"],
    requireApproval: true,
    approver: null,
    archiveAfterDays: 30,
    recipientNote: "",
  };
  /** The "brief" type's approver is the member `u-member`; "proposal" has no row, so the owner approves it. */
  const BRIEF = { label: "Brief", requireApproval: true, approver: "u-member", recipientNote: "", archiveAfterDays: 30, state: "confirmed", rev: 1 };
  function probeBody(types: unknown[] = [BRIEF]) {
    return { capability: DOCUMENT_CAPABILITY, baseline: BASELINE, types };
  }

  const file = (id: string, title: string, version: number, kind = "md") => ({
    id, version, kind, title, label: "Proposal", customer: "", bytes: 120 + version, digest: `digest-${id}`,
    created: "2026-11-10T08:00:00Z", issue: null, shared: false,
  });
  const doc = (stage: string, over: Record<string, unknown> = {}) => ({
    type: "proposal", stage, submittedVersion: null, approvedVersion: null, selfApproved: false,
    deliveredVersion: null, deliveredAt: null, archivedAt: null, rev: 1, ...over,
  });
  const DELIVERED_AT = Date.UTC(2026, 10, 12, 12);
  const ARCHIVED_AT = Date.UTC(2026, 9, 1, 12);
  const ACTIVE = [
    // A version 3 was saved after version 2 went to review (I1): the approver
    // opens version 2, whose size and digest the list does not carry.
    { file: file("art-review", "Quarterly proposal", 3), document: doc("in_review", { submittedVersion: 2 }) },
    { file: file("art-brief", "Launch brief", 1), document: doc("in_review", { type: "brief", submittedVersion: 1 }) },
    { file: file("art-approved", "Signed scope", 1), document: doc("approved", { submittedVersion: 1, approvedVersion: 1 }) },
    { file: file("art-delivered", "Delivered deck", 3), document: doc("delivered", { approvedVersion: 3, deliveredVersion: 3, deliveredAt: DELIVERED_AT }) },
    { file: file("art-draft", "Early draft", 1, "docx"), document: doc("draft") },
  ];
  const ARCHIVED = [
    { file: file("art-old", "Old contract", 1), document: doc("archived", { archivedAt: ARCHIVED_AT, deliveredAt: DELIVERED_AT }) },
    { file: file("art-memo", "Older memo", 1), document: doc("archived", { archivedAt: ARCHIVED_AT }) },
  ];

  // ---- mounting ----

  const OWNER = documentsViewer({ sub: "u-owner", operator: true, readOnly: false });
  const MEMBER = documentsViewer({ sub: "u-member", operator: false, readOnly: false });
  const OPERATOR = documentsViewer({ sub: "operator-token:ops", operator: true, readOnly: false });
  const hrefFor = (route: Route) => locationHref(goTo(readLocation("/documents", ""), route), "");

  const host = document.createElement("div");
  document.body.append(host);
  let root = createRoot(host);
  const flush = () => React.act(async () => { await new Promise((resolve) => setTimeout(resolve, 0)); });
  async function settle(check: () => void, what: string) {
    let last: unknown;
    for (let i = 0; i < 60; i++) {
      await flush();
      try { check(); return; } catch (error) { last = error; }
    }
    throw new Error(`${what}: ${last instanceof Error ? last.message : String(last)}`);
  }

  type Props = { hosted?: boolean; viewer?: Viewer; archived?: boolean; stage?: "draft" | "in_review" | "approved" | "delivered" };
  async function render(props: Props = {}) {
    await React.act(async () => {
      root.render(
        React.createElement(
          LocaleProvider,
          null,
          React.createElement(NavigationProvider, {
            adapter: hostNavigation,
            children: React.createElement(DocumentsScreen, {
              hosted: props.hosted ?? true,
              viewer: props.viewer ?? OWNER,
              archived: props.archived ?? false,
              ...(props.stage ? { stage: props.stage } : {}),
              hrefFor,
            }),
          }),
        ),
      );
    });
  }

  /** A clean mount: a new private session (the probe store forgets its latch) and a new canvas. */
  async function fresh(props: Props = {}, probe = true) {
    await React.act(async () => { root.unmount(); });
    root = createRoot(host);
    resetPrivateSession();
    allowPrivateReads();
    resetCanvas();
    calls = [];
    probeReply = () => json(probeBody());
    listReply = (archived) => json({ documents: archived ? ARCHIVED : ACTIVE });
    writeReply = () => refusal(500, "internal");
    await render(props);
    // App owns the probe; the screen only reads the shared store.
    if (probe) await React.act(async () => { await documentsProbe.revalidate(); });
  }

  const text = () => host.textContent ?? "";
  const gets = (prefix: string) => calls.filter((call) => call.method === "GET" && call.path.startsWith(prefix)).length;
  const row = (id: string) => host.querySelector(`tr[data-document-id="${id}"]`) as HTMLTableRowElement | null;
  const rowIds = () => Array.from(host.querySelectorAll("tr[data-document-id]")).map((tr) => tr.getAttribute("data-document-id"));
  const buttonIn = (scope: Element | null, label: string) =>
    Array.from(scope?.querySelectorAll("button") ?? []).find((button) => button.textContent?.trim() === label) as HTMLButtonElement | undefined;
  const dialog = () => host.querySelector('[role="dialog"]') as HTMLElement | null;
  const confirmButton = (label: string) => buttonIn(dialog(), label);
  async function click(element: Element | null | undefined) {
    ok(element, "the element to click exists");
    await React.act(async () => {
      (element as HTMLElement).focus();
      (element as HTMLElement).click();
    });
    await flush();
  }
  const listed = () => settle(() => ok(row("art-review"), "the list rendered"), "list");
  const heading = () => host.querySelector("#documents-title") as HTMLElement | null;
  const headingFocused = () => document.activeElement !== null && document.activeElement === heading();

  // ---- unavailable ----

  // A native board never probes and never lists, and a direct link shows the
  // unavailable state rather than a blank body or an error.
  await fresh({ hosted: false }, false);
  await settle(() => ok(text().includes("Documents aren’t available here"), "native board shows unavailable"), "native");
  equal(calls.length, 0, "a native board makes no document request");
  ok(!host.querySelector("[aria-labelledby]"), "the unavailable state names no missing heading");

  // A hosted board whose probe says 404 is unavailable too, and lists nothing.
  await fresh({}, false);
  probeReply = () => refusal(404, "not_found");
  await React.act(async () => { await documentsProbe.revalidate(); });
  await settle(() => ok(text().includes("Documents aren’t available here"), "a 404 probe shows unavailable"), "probe 404");
  equal(gets("/api/documents"), 0, "an unavailable board is never listed");

  // ---- checking, and a probe that failed ----

  await fresh({}, false);
  await settle(() => ok(text().includes("Checking documents…"), "the probe in flight reads as checking"), "checking");
  equal(gets("/api/documents"), 0, "nothing is listed before the probe answers");
  ok(host.querySelector('[role="status"]'), "checking is a polite status");

  probeReply = () => refusal(500, "internal");
  await React.act(async () => { await documentsProbe.revalidate(); });
  // A failed probe is not a capability answer: the screen is unavailable for
  // now, never an error, and offers a retry of the probe.
  await settle(() => ok(text().includes("Couldn’t check whether documents are available."), "a failed probe says so"), "probe error");
  ok(text().includes("Documents aren’t available here"), "a failed probe shows the unavailable state");
  ok(!host.querySelector('[role="alert"]'), "a failed probe is not an error alert");
  equal(gets("/api/documents"), 0, "a failed probe lists nothing");
  probeReply = () => json(probeBody());
  const probesBefore = gets("/api/document-types");
  await click(buttonIn(host, "Retry"));
  await listed();
  equal(gets("/api/document-types"), probesBefore + 1, "Retry asks the probe again");

  // ---- loading and a failed list ----

  await fresh({}, false);
  let release: () => void = () => {};
  listReply = (archived) => new Promise<Response>((resolve) => { release = () => resolve(json({ documents: archived ? ARCHIVED : ACTIVE })); });
  await React.act(async () => { await documentsProbe.revalidate(); });
  await settle(() => ok(text().includes("Loading documents…"), "the list in flight reads as loading"), "loading");
  await React.act(async () => { release(); });
  await listed();

  await fresh({}, false);
  listReply = () => refusal(500, "internal");
  await React.act(async () => { await documentsProbe.revalidate(); });
  await settle(() => ok(text().includes("Couldn’t load documents."), "a failed list is an error"), "list error");
  ok(host.querySelector('[role="alert"]'), "a failed list is an alert");
  listReply = (archived) => json({ documents: archived ? ARCHIVED : ACTIVE });
  const listsBefore = gets("/api/documents");
  await click(buttonIn(host, "Retry"));
  await listed();
  equal(gets("/api/documents"), listsBefore + 1, "Retry lists again");

  // A list that answers 404 after a successful probe means the lifecycle was
  // turned off mid-session: unavailable, not an error.
  await fresh({}, false);
  listReply = () => refusal(404, "not_found");
  await React.act(async () => { await documentsProbe.revalidate(); });
  await settle(() => ok(text().includes("Documents aren’t available here"), "a 404 list shows unavailable"), "list 404");
  ok(!text().includes("Couldn’t load documents."), "a 404 list is not an error");
  // The screen asks the shared probe once; its answer may list once more, and then it stops.
  await settle(() => equal(gets("/api/documents"), 2, "the probe's answer lists once more"), "list 404 settles");
  for (let i = 0; i < 5; i++) await flush();
  equal([gets("/api/documents"), gets("/api/document-types")], [2, 2], "a lasting 404 does not loop");
  ok(text().includes("Documents aren’t available here"), "a lasting 404 stays unavailable");
  // A later probe answering available again clears it (10).
  listReply = (archived) => json({ documents: archived ? ARCHIVED : ACTIVE });
  await React.act(async () => { await documentsProbe.refresh(); });
  await listed();
  ok(!text().includes("Documents aren’t available here"), "an available probe clears the unavailable state");

  // ---- empty ----

  await fresh({}, false);
  listReply = () => json({ documents: [] });
  await React.act(async () => { await documentsProbe.revalidate(); });
  await settle(() => ok(text().includes("No documents to show"), "an empty Active tab"), "empty active");
  ok(text().includes("Only the most recent files are listed, so older documents may not appear here."), "the empty Active tab does not overclaim");

  await fresh({ archived: true }, false);
  listReply = () => json({ documents: [] });
  await React.act(async () => { await documentsProbe.revalidate(); });
  await settle(() => ok(text().includes("Nothing archived"), "an empty Archived tab"), "empty archived");
  ok(calls.some((call) => call.path === "/api/documents?archived=1"), "the Archived tab lists archived documents");

  // ---- the list, tabs and stage chips ----

  await fresh();
  await listed();
  ok(calls.some((call) => call.path === "/api/documents?archived=0"), "the Active tab lists active documents");
  equal(rowIds(), ["art-review", "art-brief", "art-approved", "art-delivered", "art-draft"], "every active row, in the server's order");
  const review = row("art-review")!;
  ok(review.textContent?.includes("Quarterly proposal"), "the title");
  ok(Array.from(review.querySelectorAll("td")).some((td) => td.textContent === "proposal"), "the type");
  ok(!Array.from(host.querySelectorAll("th")).some((th) => th.textContent === "Archived"), "the Active tab has no archived column");
  ok(review.textContent?.includes("In review"), "the stage");
  ok(review.textContent?.includes("v2"), "the version under review");
  ok(!row("art-approved")!.textContent?.includes("v1"), "a version under review shows only while in review");
  ok(host.querySelector("table.stable.documents-table"), "the table keeps a readable floor and scrolls in its wrapper at 390px");
  ok(host.querySelector(".stable-wrap table.documents-table"), "inside the shared scrolling wrapper");
  // Browser QA: headers and stage chips break mid-word ("In revie/w") when the
  // shared cells' `overflow-wrap: anywhere` reaches them. Only the title cell
  // wraps, at word boundaries; the floor fits six columns.
  ok(review.querySelector("td.documents-title-cell button"), "the title sits in the one cell that wraps");
  equal(
    Array.from(review.querySelectorAll("td")).filter((td) => td.classList.contains("documents-title-cell")).length,
    1,
    "only the title cell wraps",
  );
  {
    const css: string = require("fs").readFileSync(
      require("path").resolve("src/features/documents/documents.css"),
      "utf8",
    );
    const rule = (selector: string) => {
      const at = css.indexOf(`${selector} {`);
      return at < 0 ? "" : css.slice(at, css.indexOf("}", at));
    };
    const floor = Number(/min-width:\s*(\d+)px/.exec(rule(".documents-table"))?.[1] ?? 0);
    ok(floor >= 760, `the table's floor fits six columns (${floor}px)`);
    for (const selector of [".documents-table th", ".documents-table .chip"]) {
      ok(/white-space:\s*nowrap/.test(rule(selector)), `${selector} keeps its words whole`);
      ok(/overflow-wrap:\s*normal/.test(rule(selector)), `${selector} never breaks inside a word`);
    }
    ok(/overflow-wrap:\s*normal/.test(rule(".documents-table td")), "cells wrap only at word boundaries");
    ok(/overflow-wrap:\s*break-word/.test(rule(".documents-table td.documents-title-cell")), "a title breaks a word only when it alone overflows");
  }
  equal([heading()?.tagName, heading()?.tabIndex, heading()?.className.includes("font-semibold")], ["H1", -1, true], "the heading can take focus");
  ok(Array.from(host.querySelectorAll("th")).some((th) => th.textContent === "Version under review"), "the column says what it counts");
  ok(review.querySelector(".chip")?.textContent?.includes("In review"), "the stage is a chip");
  ok(row("art-delivered")!.textContent?.includes("2026"), "the delivered date");
  ok(!text().includes("Share"), "the screen never offers sharing");

  const tabs = Array.from(host.querySelectorAll("nav.section-nav a")) as HTMLAnchorElement[];
  equal(tabs.map((tab) => [tab.textContent, tab.getAttribute("href"), tab.getAttribute("aria-current")]), [
    ["Active", "/documents", "page"],
    ["Archived", "/documents?archived=1", null],
  ], "the Active and Archived tabs");

  const chips = () => Array.from(host.querySelectorAll('nav[data-stage-filter] a')) as HTMLAnchorElement[];
  equal(chips().map((chip) => [chip.textContent, chip.getAttribute("href")]), [
    ["All", "/documents"],
    ["In review", "/documents?stage=in_review"],
    ["Approved", "/documents?stage=approved"],
    ["Delivered", "/documents?stage=delivered"],
    ["Draft", "/documents?stage=draft"],
  ], "the stage chips carry the filter in the URL");
  equal(chips().filter((chip) => chip.getAttribute("aria-current")).map((chip) => chip.textContent), ["All"], "All is current without a filter");

  await render({ stage: "in_review" });
  await settle(() => equal(rowIds(), ["art-review", "art-brief"], "the in-review filter"), "filter");
  equal(chips().filter((chip) => chip.getAttribute("aria-current")).map((chip) => chip.textContent), ["In review"], "the current chip");
  await render({ stage: "delivered" });
  await settle(() => equal(rowIds(), ["art-delivered"], "the delivered filter"), "filter delivered");
  equal(gets("/api/documents"), 1, "filtering is client-side: no new list request");

  await fresh({ archived: true });
  await settle(() => ok(row("art-old"), "the archived row"), "archived list");
  ok(Array.from(host.querySelectorAll("th")).some((th) => th.textContent === "Archived"), "the archived date column");
  ok(row("art-old")!.textContent?.includes("2026"), "the archived date");
  equal(chips().length, 0, "the Archived tab has no stage chips");
  equal(
    (Array.from(host.querySelectorAll("nav.section-nav a")) as HTMLAnchorElement[]).map((tab) => tab.getAttribute("aria-current")),
    [null, "page"],
    "the Archived tab is current",
  );

  // A filter that matches nothing says so.
  await fresh({ stage: "delivered" }, false);
  listReply = () => json({ documents: [ACTIVE[0]] });
  await React.act(async () => { await documentsProbe.revalidate(); });
  await settle(() => ok(text().includes("No documents at this stage"), "an empty filter"), "empty filter");
  const back = Array.from(host.querySelectorAll('[data-empty="documents-filtered"] a')) as HTMLAnchorElement[];
  equal(back.map((a) => [a.textContent, a.getAttribute("href")]), [["Show all stages", "/documents"]], "an empty filter links back to All");

  // ---- opening a document ----

  await fresh();
  await listed();
  const title = buttonIn(row("art-review"), "Quarterly proposal");
  ok(title, "the title is a button, so Tab reaches it");
  await click(title);
  const canvas = canvasSnapshot();
  equal(
    [canvas.conversation, canvas.composerKey, canvas.open, canvas.active, canvas.tabs[0]?.selected],
    [DOCUMENTS_CONVERSATION, null, true, "art-review", 2],
    "a row opens its file in the canvas drawer, bound to the sentinel conversation without Ask to change",
  );
  const reviewed = canvas.tabs.find((tab) => tab.id === "art-review")!;
  equal([reviewed.bytes, reviewed.versions[2]], [undefined, {}], "version 2 shows neither version 3's size nor its digest");
  await click(buttonIn(row("art-approved"), "Signed scope"));
  const latest = canvasSnapshot().tabs.find((tab) => tab.id === "art-approved")!;
  equal([latest.selected, latest.bytes, latest.versions[1]], [1, 121, { bytes: 121, digest: "digest-art-approved" }], "the listed version keeps its own size and digest");
  ok(!buttonIn(row("art-draft"), "Early draft"), "a file the canvas cannot show is not a button");
  ok(row("art-draft")!.textContent?.includes("Early draft"), "it still shows its title");

  // A non-approver opens the newest version (v3, whose size the list carries)
  // and picks v1 in the drawer: v1's size is unknown, so the footer shows no
  // size at all, never v3's.
  await fresh({ viewer: MEMBER });
  await listed();
  const drawerHost = document.createElement("div");
  document.body.append(drawerHost);
  const drawerRoot = createRoot(drawerHost);
  await React.act(async () => {
    drawerRoot.render(React.createElement(LocaleProvider, null, React.createElement(CanvasDrawer)));
  });
  await click(buttonIn(row("art-review"), "Quarterly proposal"));
  equal(canvasSnapshot().tabs.find((tab) => tab.id === "art-review")?.selected, 3, "a non-approver opens the newest version");
  const foot = () => drawerHost.querySelector(".cv-foot span")?.textContent ?? null;
  await settle(() => ok(foot()?.startsWith("v3 · "), `v3 shows its own size (${foot()})`), "v3 size");
  await React.act(async () => { selectVersion("art-review", 1); });
  await settle(() => equal(foot(), "v1", "a picked version with no known size shows none"), "v1 size");
  await React.act(async () => { drawerRoot.unmount(); });
  drawerHost.remove();
  await React.act(async () => { root.unmount(); });
  root = createRoot(host);
  equal(canvasSnapshot().conversation, null, "leaving the screen closes its canvas");
  await fresh();
  await listed();
  await click(buttonIn(row("art-review"), "Quarterly proposal"));
  pausePrivateReads();
  await React.act(async () => { root.unmount(); });
  root = createRoot(host);
  equal(canvasSnapshot().conversation, DOCUMENTS_CONVERSATION, "a paused session leaves the canvas to the session reset");
  allowPrivateReads();
  await fresh();
  await listed();

  // ---- who sees which action ----

  // The owner approves a type with no confirmed approver, and archives.
  ok(buttonIn(row("art-review"), "Approve"), "the owner approves a baseline type");
  ok(!buttonIn(row("art-brief"), "Approve"), "the owner does not approve a type with another approver");
  ok(!buttonIn(row("art-approved"), "Approve"), "nothing approves outside review");
  for (const id of ["art-review", "art-brief", "art-approved", "art-delivered", "art-draft"])
    ok(buttonIn(row(id), "Archive"), `the owner can archive ${id}`);
  ok(!buttonIn(host, "Restore"), "the Active tab offers no restore");
  for (const button of Array.from(host.querySelectorAll("tbody button")) as HTMLButtonElement[])
    ok(button.tabIndex >= 0 && !button.disabled, "every row action is reachable by Tab");

  await fresh({ viewer: MEMBER });
  await listed();
  ok(buttonIn(row("art-brief"), "Approve"), "the type's approver approves");
  ok(!buttonIn(row("art-review"), "Approve"), "a member does not approve the owner's types");
  ok(!buttonIn(host, "Archive") && !buttonIn(host, "Restore"), "a member neither archives nor restores");

  await fresh({ viewer: OPERATOR });
  await listed();
  ok(!buttonIn(host, "Approve"), "an operator session never approves");
  ok(!buttonIn(host, "Archive"), "an operator session never archives");
  await fresh({ viewer: OPERATOR, archived: true });
  await settle(() => ok(row("art-old"), "archived list"), "operator archived");
  ok(!buttonIn(host, "Restore"), "an operator session never restores");

  // Access waits for the types: while the probe is still checking, no action shows.
  await fresh({}, false);
  ok(!buttonIn(host, "Approve") && !buttonIn(host, "Archive"), "no action before the types load");

  // ---- approve ----

  await fresh();
  await listed();
  const approve = buttonIn(row("art-review"), "Approve")!;
  await click(approve);
  ok(dialog(), "Approve asks first");
  ok(dialog()!.textContent?.includes("Quarterly proposal"), "the dialog names the document");
  ok(dialog()!.textContent?.includes("version 2"), "the dialog names the version shown");
  ok(confirmButton("Approve")!.classList.contains("btn-primary") && !confirmButton("Approve")!.classList.contains("btn-danger"), "Approve confirms with the primary button");
  // Escape closes it and returns focus to the trigger.
  await React.act(async () => {
    dialog()!.dispatchEvent(new KeyboardEvent("keydown", { key: "Escape", bubbles: true }));
  });
  await flush();
  ok(!dialog(), "Escape closes the dialog");
  equal(document.activeElement === approve, true, "focus returns to Approve");
  equal(calls.filter((call) => call.method === "POST").length, 0, "a cancelled approve sends nothing");

  writeReply = () => json({ stage: "approved", version: 2, selfApproved: true });
  listReply = (archived) => json({
    documents: archived ? ARCHIVED : ACTIVE.map((entry) =>
      entry.file.id === "art-review" ? { ...entry, document: { ...entry.document, stage: "approved", approvedVersion: 2, selfApproved: true } } : entry),
  });
  await click(buttonIn(row("art-review"), "Approve"));
  await click(confirmButton("Approve"));
  await settle(() => ok(row("art-review")!.textContent?.includes("Approved by you (self)"), "the row says who approved"), "approve success");
  const post = calls.find((call) => call.method === "POST")!;
  equal([post.path, post.body], ["/api/documents/art-review/approve", { version: 2 }], "approve sends exactly the submitted version");
  ok(row("art-review")!.querySelector(".chip")?.textContent?.includes("Approved"), "the row's stage is approved");
  ok(!buttonIn(row("art-review"), "Approve"), "an approved row has no Approve");
  ok(calls.slice(calls.indexOf(post)).some((call) => call.path === "/api/documents?archived=0"), "the list is read again");
  ok(!dialog(), "the dialog closes");
  ok(headingFocused(), "after a write focus moves to the heading");

  // Each refusal is told in the screen's own words, and the list and the
  // types are read again, so a stale button goes away.
  const approveRefusals: [number, string, string][] = [
    [409, "conflict", "This document changed since you opened it. Review the new version, then approve it."],
    [409, "not_in_review", "This document is no longer in review."],
    [409, "approver_unavailable", "This type’s approver has left the workspace. The owner must update the type before it can be approved."],
    [403, "not_approver", "You’re not the approver for this type of document."],
    [403, "self_approval", "You can’t approve a version you wrote."],
    [409, "document_archived", "This document has been archived."],
    [404, "not_found", "This document no longer exists."],
    [403, "owner_required", "Only the workspace’s own members can change documents."],
    [403, "membership_changed", "Your access changed. Reload the page and try again."],
    [409, "tenant_placement_changed", "Couldn’t approve this document. Try again."],
  ];
  await fresh();
  await listed();
  for (const [status, code, copy] of approveRefusals) {
    writeReply = () => refusal(status, code);
    await click(buttonIn(row("art-review"), "Approve"));
    const before = calls.length;
    await click(confirmButton("Approve"));
    await settle(() => ok(text().includes(copy), `${code} is told as “${copy}”`), code);
    ok(!text().includes(`server says ${code}`), `${code} does not show the server's message`);
    await settle(() => {
      const after = calls.slice(before);
      ok(after.some((call) => call.path === "/api/document-types"), `${code} reads the types again`);
      ok(after.some((call) => call.path === "/api/documents?archived=0"), `${code} reads the list again`);
    }, `${code} re-read`);
    ok(!dialog(), `${code} closes the dialog`);
    ok(headingFocused(), `after ${code} focus moves to the heading`);
  }
  // A refusal without a code is the generic message, not the server's words.
  writeReply = () => new Response("gateway down", { status: 502 });
  await click(buttonIn(row("art-review"), "Approve"));
  await click(confirmButton("Approve"));
  await settle(() => ok(text().includes("Couldn’t approve this document. Try again."), "an uncoded failure is generic"), "uncoded");

  // A write that rejects with AbortError may have been applied: no error, and
  // the list is read again.
  await fresh();
  await listed();
  writeReply = () => { throw Object.assign(new Error("The operation was aborted."), { name: "AbortError" }); };
  await click(buttonIn(row("art-review"), "Approve"));
  const beforeAbort = calls.length;
  await click(confirmButton("Approve"));
  await settle(() => ok(calls.slice(beforeAbort).some((call) => call.path === "/api/documents?archived=0"), "the list is read again after an abort"), "abort");
  ok(!dialog(), "an aborted approve closes the dialog");
  ok(!host.querySelector('[role="alert"]'), "an aborted approve shows no error");
  ok(!text().includes("Couldn’t approve"), "an aborted approve is not reported as a failure");

  // The real path: the session pauses while the write is in flight. The client
  // rejects with an AbortError, and the screen does not list into a paused
  // session (that read would fail and read as an error).
  await fresh();
  await listed();
  writeReply = () => {
    pausePrivateReads();
    return json({ stage: "approved", version: 2, selfApproved: false });
  };
  await click(buttonIn(row("art-review"), "Approve"));
  const beforePause = calls.length;
  await click(confirmButton("Approve"));
  await settle(() => ok(!dialog(), "a paused write closes the dialog"), "paused write");
  for (let i = 0; i < 5; i++) await flush();
  equal(calls.slice(beforePause).map((call) => call.method), ["POST"], "nothing is read into a paused session");
  ok(!host.querySelector('[role="alert"]'), "a paused write shows no error");
  ok(!text().includes("Couldn’t refresh this list"), "a paused write does not report a failed refresh");
  ok(row("art-review"), "the rows stay");
  allowPrivateReads();

  // ---- archive and restore ----

  await fresh();
  await listed();
  writeReply = () => json({ stage: "archived" });
  await click(buttonIn(row("art-delivered"), "Archive"));
  ok(dialog()?.textContent?.includes("Delivered deck"), "Archive asks first, naming the document");
  ok(confirmButton("Archive")!.classList.contains("btn-danger"), "Archive confirms with the danger button");
  // Hold the re-read, so the row leaving is the screen's doing, not the server's.
  let releaseArchive: () => void = () => {};
  listReply = (archived) => new Promise<Response>((resolve) => { releaseArchive = () => resolve(json({ documents: archived ? ARCHIVED : ACTIVE.filter((entry) => entry.file.id !== "art-delivered") })); });
  await click(confirmButton("Archive"));
  await settle(() => ok(text().includes("Document archived."), "archive succeeds"), "archive");
  ok(!row("art-delivered"), "an archived row leaves the Active tab at once");
  ok(headingFocused(), "after archiving focus moves to the heading");
  await React.act(async () => { releaseArchive(); });
  await flush();
  listReply = (archived) => json({ documents: archived ? ARCHIVED : ACTIVE.filter((entry) => entry.file.id !== "art-delivered") });
  equal(calls.filter((call) => call.method === "POST").map((call) => call.path), ["/api/documents/art-delivered/archive"], "archive posts once");

  const archiveRefusals: [number, string, string][] = [
    [403, "owner_only", "Only the workspace owner can archive documents."],
    [404, "not_found", "This document no longer exists."],
    [500, "internal", "Couldn’t archive this document. Try again."],
  ];
  for (const [status, code, copy] of archiveRefusals) {
    writeReply = () => refusal(status, code);
    await click(buttonIn(row("art-draft"), "Archive"));
    await click(confirmButton("Archive"));
    await settle(() => ok(text().includes(copy), `archive ${code} is told as “${copy}”`), `archive ${code}`);
  }

  await fresh({ archived: true });
  await settle(() => ok(row("art-old"), "archived list"), "restore list");
  ok(!buttonIn(host, "Archive"), "the Archived tab offers no archive");
  writeReply = () => json({ stage: "delivered" });
  const restore = buttonIn(row("art-old"), "Restore")!;
  await click(restore);
  ok(dialog()?.textContent?.includes("Old contract"), "Restore asks first, naming the document");
  ok(dialog()?.textContent?.includes("Nothing is re-run."), "Restore says nothing is re-run");
  ok(confirmButton("Restore")!.classList.contains("btn-primary"), "Restore confirms with the primary button");
  await React.act(async () => {
    dialog()!.dispatchEvent(new KeyboardEvent("keydown", { key: "Escape", bubbles: true }));
  });
  await flush();
  equal([dialog(), document.activeElement === restore], [null, true], "Escape closes Restore and returns focus");
  await click(restore);
  let releaseRestore: () => void = () => {};
  listReply = (archived) => new Promise<Response>((resolve) => { releaseRestore = () => resolve(json({ documents: archived ? ARCHIVED.filter((entry) => entry.file.id !== "art-old") : ACTIVE })); });
  await click(confirmButton("Restore"));
  await settle(() => ok(text().includes("Document restored."), "restore succeeds"), "restore");
  ok(!row("art-old"), "a restored row leaves the Archived tab at once");
  ok(headingFocused(), "after restoring focus moves to the heading");
  await React.act(async () => { releaseRestore(); });
  await flush();
  listReply = (archived) => json({ documents: archived ? ARCHIVED.filter((entry) => entry.file.id !== "art-old") : ACTIVE });
  equal(calls.filter((call) => call.method === "POST").map((call) => call.path), ["/api/documents/art-old/restore"], "restore posts once");

  writeReply = () => refusal(403, "owner_only");
  await click(buttonIn(row("art-memo"), "Restore"));
  await click(confirmButton("Restore"));
  await settle(() => ok(text().includes("Only the workspace owner can restore documents."), "restore owner_only"), "restore refusal");

  writeReply = () => { throw Object.assign(new Error("aborted"), { name: "AbortError" }); };
  await click(buttonIn(row("art-memo"), "Restore"));
  const beforeRestoreAbort = calls.length;
  await click(confirmButton("Restore"));
  await settle(() => ok(calls.slice(beforeRestoreAbort).some((call) => call.path === "/api/documents?archived=1"), "an aborted restore reads the list again"), "restore abort");
  ok(!dialog(), "an aborted restore closes the dialog");

  // ---- the copy is translated ----

  const strings = [
    "Documents aren’t available here",
    "Checking documents…",
    "Couldn’t check whether documents are available.",
    "Loading documents…",
    "Couldn’t load documents.",
    "No documents to show",
    "Only the most recent files are listed, so older documents may not appear here.",
    "Show all stages",
    "This workspace doesn’t have document review turned on.",
    "Document",
    "Document type",
    "Stage",
    "Version under review",
    "Filter by stage",
    "Active documents",
    "Archived documents",
    "Approved by you",
    "Approve {title}",
    "Archive {title}",
    "Restore {title}",
    "Approve document?",
    "Archive document?",
    "Restore document?",
    "Approve version {version} of “{title}”? A later version needs its own approval.",
    "Archive “{title}”? It moves to the Archived tab, and the owner can restore it.",
    "Restore “{title}”? It returns to the stage it had before it was archived. Nothing is re-run.",
    "Document approved.",
    "Couldn’t refresh this list, so it may be out of date.",
    "Retry",
    "Home",
    "Actions",
    "Nothing archived",
    "No documents at this stage",
    "Active",
    "Archived",
    "In review",
    "Approved",
    "Delivered",
    "Draft",
    "Approve",
    "Archive",
    "Restore",
    "Approved by you (self)",
    "Document archived.",
    "Document restored.",
    "Only the workspace owner can archive documents.",
    "Only the workspace owner can restore documents.",
    "Couldn’t archive this document. Try again.",
    "Couldn’t restore this document. Try again.",
    ...approveRefusals.map(([, , copy]) => copy),
  ];
  for (const english of strings) ok(translateUiText(english, "zh-TW") !== english, `zh-TW has “${english}”`);
  // The tab and the region it labels read the same word (15).
  ok(translateUiText("Active documents", "zh-TW").startsWith(translateUiText("Active", "zh-TW")), "Active reads the same in the tab and the table label");

  await React.act(async () => { root.unmount(); });
  console.log("documents screen checks passed");
}

void main().catch((err: unknown) => {
  setTimeout(() => {
    throw err;
  }, 0);
});
