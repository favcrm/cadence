/**
 * CAD-2286: Settings → Documents, mounted in happy-dom against a fake board.
 *
 * It covers each state (unavailable, unsupported, checking access, loading, a
 * failed probe, the baseline only, pending and confirmed rows), who may change
 * a type (the owner; never a member, a read-only board or an operator
 * session), the form's checks against the server's `validTypeInput`, the exact
 * request bodies, each refusal as translated copy, a conflict that reads the
 * types again, the roster (names, an unreadable roster, `approver_unavailable`
 * and a 503 `capability_unavailable`), and a draft that survives a re-read.
 */
declare function require(name: string): any;
export {};

function ok(value: unknown, why: string): asserts value {
  if (!value) throw new Error(why);
}
/** Focus is on `expected` (elements are compared by identity, never serialized). */
function focused(expected: Element | null | undefined, why: string): void {
  if (!expected || document.activeElement !== expected)
    throw new Error(`${why}: focus is on ${document.activeElement?.outerHTML.slice(0, 120) ?? "nothing"}`);
}
function equal(actual: unknown, expected: unknown, why: string): void {
  const a = JSON.stringify(actual);
  const e = JSON.stringify(expected);
  if (a !== e) throw new Error(`${why}: expected ${e}, got ${a}`);
}

async function main() {
  const { Window } = require("happy-dom");
  const win = new Window({ url: "http://localhost/settings/documents" });
  for (const name of [
    "window", "document", "Node", "Element", "HTMLElement", "HTMLInputElement", "HTMLButtonElement", "SVGElement",
    "navigator", "MutationObserver", "Event", "MouseEvent", "KeyboardEvent", "PointerEvent", "location", "history",
    "localStorage", "sessionStorage",
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
  const pageModule = require("../src/features/settings/DocumentTypes") as typeof import("../src/features/settings/DocumentTypes");
  const DocumentTypes = pageModule.default;
  const { validateTypeDraft, draftInput } = pageModule;
  const { DOCUMENT_CAPABILITY } = require("../src/features/documents/documentsClient") as typeof import("../src/features/documents/documentsClient");
  const { documentsProbe } = require("../src/features/documents/documentsProbe") as typeof import("../src/features/documents/documentsProbe");
  const { allowPrivateReads, resetPrivateSession } = require("../src/lib/privateSession") as typeof import("../src/lib/privateSession");
  const { NavigationProvider } = require("../screen-ui/src/navigation") as typeof import("../screen-ui/src/navigation");
  const { hostNavigation } = require("../src/ui/hostNavigation") as typeof import("../src/ui/hostNavigation");
  type Viewer = import("../src/features/projects/work").Viewer;
  type TypeDraft = import("../src/features/settings/DocumentTypes").TypeDraft;

  // ---- the fake board ----

  type Reply = Response | Promise<Response>;
  const json = (value: unknown, status = 200) =>
    new Response(JSON.stringify(value), { status, headers: { "Content-Type": "application/json" } });
  const refusal = (status: number, code: string) => json({ ok: false, error: { code, message: `server says ${code}` } }, status);
  const abortError = () => Object.assign(new Error("The operation was aborted."), { name: "AbortError" });

  type Call = { method: string; path: string; body: unknown; headers: Record<string, string> };
  let calls: Call[] = [];
  let probeReply: () => Reply = () => json(probeBody());
  let rosterReply: () => Reply = () => json({ members: ROSTER });
  let writeReply: (path: string, body: unknown) => Reply = () => refusal(500, "internal");
  globalThis.fetch = (async (input: unknown, init?: RequestInit) => {
    const path = String(input);
    const method = init?.method ?? "GET";
    const body = typeof init?.body === "string" ? JSON.parse(init.body) : null;
    calls.push({ method, path, body, headers: { ...(init?.headers as Record<string, string> | undefined) } });
    if (method === "GET" && path === "/api/document-types") return probeReply();
    if (method === "GET" && path === "/api/team/members") return rosterReply();
    if (method === "POST" && path.startsWith("/api/document-types/")) return writeReply(path, body);
    return refusal(404, "not_found");
  }) as typeof fetch;

  const BASELINE = {
    stages: ["draft", "in_review", "approved", "delivered", "archived"],
    requireApproval: true,
    approver: null,
    archiveAfterDays: 30,
    recipientNote: "",
  };
  const ROSTER = [
    { userId: "u-owner", name: "Olivia Owner", email: "olivia@example.com", role: "owner", joinedAt: "2026-01-01T00:00:00Z" },
    { userId: "u-ann", name: "Ann Lee", email: "ann@example.com", role: "member", joinedAt: "2026-02-01T00:00:00Z" },
    { userId: "u-ben", name: "", email: "ben@example.com", role: "member", joinedAt: "2026-03-01T00:00:00Z" },
  ];
  /** brief: confirmed, approved by Ann, with a note that looks like markup.
   *  contract: a member's pending proposal. memo: confirmed, approver has left. */
  const NOTE = "Reply within <b>two</b> **days**";
  const BRIEF = { label: "brief", requireApproval: true, approver: "u-ann", recipientNote: NOTE, archiveAfterDays: 14, state: "confirmed", rev: 2 };
  const CONTRACT = { label: "contract", requireApproval: false, approver: null, recipientNote: "", archiveAfterDays: 90, state: "pending", rev: 1 };
  const MEMO = { label: "memo", requireApproval: true, approver: "u-gone", recipientNote: "", archiveAfterDays: 1, state: "confirmed", rev: 3 };
  function probeBody(types: unknown[] = [BRIEF, CONTRACT, MEMO]) {
    return { capability: DOCUMENT_CAPABILITY, baseline: BASELINE, types };
  }

  // ---- mounting ----

  const OWNER: Viewer = { operator: true, readOnly: false };
  const MEMBER: Viewer = { operator: false, readOnly: false };
  const READ_ONLY_OWNER: Viewer = { operator: true, readOnly: true };
  const CHECKING: Viewer = { operator: null, readOnly: false, access: "checking" };
  let accessRetries = 0;
  const UNCONFIRMED: Viewer = { operator: null, readOnly: false, access: "unavailable", onRetryAccess: () => { accessRetries += 1; } };

  const host = document.createElement("div");
  document.body.append(host);
  let root = createRoot(host);
  const flush = () => React.act(async () => { await new Promise((resolve) => setTimeout(resolve, 0)); });
  /** A drawer's close waits out its exit transition (a 320ms fallback here). */
  const closed = () => React.act(async () => { await new Promise((resolve) => setTimeout(resolve, 400)); });
  async function settle(check: () => void, what: string) {
    let last: unknown;
    for (let i = 0; i < 60; i++) {
      await flush();
      try { check(); return; } catch (error) { last = error; }
    }
    throw new Error(`${what}: ${last instanceof Error ? last.message : String(last)}`);
  }

  type Props = { hosted?: boolean; viewer?: Viewer; sub?: string | null };
  async function render(props: Props = {}) {
    await React.act(async () => {
      root.render(
        React.createElement(
          LocaleProvider,
          null,
          React.createElement(NavigationProvider, {
            adapter: hostNavigation,
            children: React.createElement(DocumentTypes, {
              hosted: props.hosted ?? true,
              viewer: props.viewer ?? OWNER,
              viewerSub: props.sub === undefined ? "u-owner" : props.sub,
            }),
          }),
        ),
      );
    });
  }

  /** A clean mount: a new private session (the probe store forgets its latch). */
  async function fresh(props: Props = {}, probe = true) {
    await React.act(async () => { root.unmount(); });
    document.body.replaceChildren(host);
    host.replaceChildren();
    root = createRoot(host);
    resetPrivateSession();
    allowPrivateReads();
    calls = [];
    probeReply = () => json(probeBody());
    rosterReply = () => json({ members: ROSTER });
    writeReply = () => refusal(500, "internal");
    await render(props);
    // App owns the probe; the page only reads the shared store.
    if (probe) await React.act(async () => { await documentsProbe.revalidate(); });
  }

  const text = () => document.body.textContent ?? "";
  const gets = (path: string) => calls.filter((call) => call.method === "GET" && call.path === path).length;
  const posts = () => calls.filter((call) => call.method === "POST");
  const row = (label: string) => host.querySelector(`li[data-type-label="${label}"]`) as HTMLElement | null;
  const rowLabels = () => Array.from(host.querySelectorAll("li[data-type-label]")).map((li) => li.getAttribute("data-type-label"));
  const value = (scope: Element | null, term: string) => {
    const pair = Array.from(scope?.querySelectorAll("dl > div") ?? []).find((div) => div.querySelector("dt")?.textContent === term);
    return pair?.querySelector("dd")?.textContent ?? null;
  };
  const buttonIn = (scope: Element | null, label: string) =>
    Array.from(scope?.querySelectorAll("button") ?? []).find((button) => button.textContent?.trim() === label) as HTMLButtonElement | undefined;
  const dialog = () => document.querySelector('[role="dialog"][aria-modal="true"]') as HTMLElement | null;
  const drawer = () => document.querySelector('[data-drawer="document-type"]') as HTMLElement | null;
  const field = (label: string) => {
    const element = Array.from(drawer()?.querySelectorAll("label") ?? []).find((node) => node.textContent?.replace(" *", "").trim() === label);
    const id = element?.getAttribute("for");
    return (id ? document.getElementById(id) : null) as HTMLInputElement | null;
  };
  /** The Field's error under a control (`<id>-error`, `role="alert"`). */
  const errorFor = (label: string) => {
    const id = field(label)?.id;
    const node = id ? document.getElementById(`${id}-error`) : null;
    return node?.getAttribute("role") === "alert" ? node.textContent : null;
  };
  const checkbox = () => drawer()?.querySelector('input[type="checkbox"]') as HTMLInputElement | null;
  const approverTrigger = () => drawer()?.querySelector('button[role="combobox"]') as HTMLButtonElement | null;
  const status = () => Array.from(document.querySelectorAll('[role="status"]')).map((node) => node.textContent).join(" | ");
  async function click(element: Element | null | undefined) {
    ok(element, "the element to click exists");
    await React.act(async () => {
      (element as HTMLElement).focus();
      (element as HTMLElement).click();
    });
    await flush();
  }
  async function fill(input: HTMLInputElement | null, next: string) {
    ok(input, "the field exists");
    await React.act(async () => {
      Object.getOwnPropertyDescriptor(win.HTMLInputElement.prototype, "value")!.set!.call(input, next);
      input.dispatchEvent(new win.Event("input", { bubbles: true }));
    });
    await flush();
  }
  async function chooseApprover(label: string) {
    await click(approverTrigger());
    const option = Array.from(document.querySelectorAll('[role="option"]')).find((node) => node.querySelector(".select-label")?.textContent === label);
    ok(option, `the approver list offers ${label}`);
    await click(option);
  }
  async function save() {
    await click(buttonIn(drawer(), "Save"));
    await flush();
  }
  const listed = () => settle(() => ok(row("brief"), "the types rendered"), "types");
  /** After a refused or failed save the drawer stays open and focus stays in it
   *  (Browser QA): on the Approver for `approver_unavailable`, which is marked
   *  invalid and described by its error; on the form's error otherwise. */
  function expectSaveFocus(why: string, copy: string, atApprover: boolean) {
    const trigger = approverTrigger();
    ok(trigger, `${why}: the drawer is still open`);
    ok(drawer()!.contains(document.activeElement), `${why}: focus stays in the drawer (it is on ${document.activeElement?.tagName})`);
    if (atApprover) {
      focused(trigger, `${why} moves focus to the Approver`);
      equal(trigger.getAttribute("aria-invalid"), "true", `${why} marks the Approver invalid`);
      const described = (trigger.getAttribute("aria-describedby") ?? "").split(" ").map((id) => document.getElementById(id)?.textContent);
      ok(described.includes(copy), `${why}: the Approver is described by its error (${JSON.stringify(described)})`);
    } else {
      const error = drawer()!.querySelector<HTMLElement>("[data-form-error]");
      ok(error?.textContent?.includes(copy), `${why}: the form's error holds the copy`);
      focused(error, `${why} moves focus to the form's error`);
      equal(trigger.getAttribute("aria-invalid"), null, `${why} does not mark the Approver invalid`);
    }
  }
  const heading = () => host.querySelector("#document-types-title") as HTMLElement | null;

  // ---- the form's checks mirror the server's validTypeInput ----

  const t = (english: string) => english;
  const draft = (over: Partial<TypeDraft> = {}): TypeDraft => ({
    label: "Proposal", requireApproval: true, approver: "", recipientNote: "", archiveAfterDays: "30", ...over,
  });
  equal(validateTypeDraft(draft(), true, ["brief"], t), {}, "a good draft passes");
  equal(validateTypeDraft(draft({ label: "  " }), true, [], t), { label: "Enter a name for this type." }, "an empty name");
  equal(validateTypeDraft(draft({ label: "x".repeat(40) }), true, [], t), {}, "40 characters is the limit");
  equal(validateTypeDraft(draft({ label: ` ${"x".repeat(40)} ` }), true, [], t), {}, "the limit counts the trimmed name");
  equal(validateTypeDraft(draft({ label: "x".repeat(41) }), true, [], t), { label: "Use 40 characters or fewer." }, "41 characters");
  equal(validateTypeDraft(draft({ label: "a\tb" }), true, [], t), { label: "Remove line breaks and other control characters." }, "a tab in the name");
  equal(validateTypeDraft(draft({ label: "a\nb" }), true, [], t), { label: "Remove line breaks and other control characters." }, "a newline in the name");
  equal(validateTypeDraft(draft({ label: "a\u007fb" }), true, [], t), { label: "Remove line breaks and other control characters." }, "DEL in the name");
  equal(validateTypeDraft(draft({ label: " BRIEF " }), true, ["brief"], t), { label: "This type already exists. Edit it from the list instead." }, "an existing key, in any case");
  equal(validateTypeDraft(draft({ label: "" }), false, ["brief"], t), {}, "an edit does not check the name");
  // AOS-590: the key is one URL path segment, as the server's validTypeInput
  // reads it: a name of only dots, or one with a slash, is refused.
  for (const label of [".", "..", " .. ", "..."])
    equal(validateTypeDraft(draft({ label }), true, [], t), { label: "Use a name that isn’t only dots." }, `${JSON.stringify(label)} is refused`);
  for (const label of ["a/b", "a\\b", "/", " \\ "])
    equal(validateTypeDraft(draft({ label }), true, [], t), { label: "Use a name without “/” or “\\”." }, `${JSON.stringify(label)} is refused`);
  for (const label of ["v1.2", "q.a"])
    equal(validateTypeDraft(draft({ label }), true, [], t), {}, `${JSON.stringify(label)} is an ordinary name`);
  equal(validateTypeDraft(draft({ recipientNote: "n".repeat(200) }), true, [], t), {}, "a 200-character note");
  equal(validateTypeDraft(draft({ recipientNote: "n".repeat(201) }), true, [], t), { recipientNote: "Use 200 characters or fewer." }, "a 201-character note");
  equal(validateTypeDraft(draft({ recipientNote: "a\nb" }), true, [], t), { recipientNote: "Remove line breaks and other control characters." }, "a note is one line");
  equal(validateTypeDraft(draft({ recipientNote: "a\tb" }), true, [], t), { recipientNote: "Remove line breaks and other control characters." }, "a tab in the note");
  equal(validateTypeDraft(draft({ approver: "u\nx" }), true, [], t), { approver: "Choose an approver from the list." }, "an approver with a control character");
  equal(validateTypeDraft(draft({ approver: "u".repeat(201) }), true, [], t), { approver: "Choose an approver from the list." }, "an approver over 200 characters");
  for (const days of ["1", "3650", " 45 "])
    equal(validateTypeDraft(draft({ archiveAfterDays: days }), true, [], t), {}, `${JSON.stringify(days)} days`);
  for (const days of ["0", "3651", "", "1.5", "1e3", "+5", "-1", "ten"])
    equal(
      validateTypeDraft(draft({ archiveAfterDays: days }), true, [], t),
      { archiveAfterDays: "Enter a whole number of days from 1 to 3650." },
      `${JSON.stringify(days)} days is refused`,
    );
  equal(
    draftInput(draft({ approver: "", recipientNote: " keep my spaces ", archiveAfterDays: " 45 " })),
    { requireApproval: true, approver: null, recipientNote: " keep my spaces ", archiveAfterDays: 45 },
    "the workspace owner is a null approver, and the note is sent as typed",
  );

  // ---- unavailable and unsupported ----

  // A native board never reads the probe or the roster.
  await fresh({ hosted: false }, false);
  await settle(() => ok(text().includes("Document types aren’t available here"), "native board shows unavailable"), "native");
  ok(text().includes("This workspace doesn’t have document review turned on."), "native says why");
  equal(calls.length, 0, "a native board makes no request");

  await fresh({}, false);
  probeReply = () => refusal(404, "not_found");
  await React.act(async () => { await documentsProbe.revalidate(); });
  await settle(() => ok(text().includes("This workspace doesn’t have document review turned on."), "a 404 probe is unavailable"), "probe 404");
  ok(!text().includes("Default for new types"), "an unavailable board invents no baseline");
  equal(gets("/api/team/members"), 0, "an unavailable board reads no roster");

  await fresh({}, false);
  probeReply = () => json({ capability: "cad2286.document-lifecycle.v2", baseline: BASELINE, types: [] });
  await React.act(async () => { await documentsProbe.revalidate(); });
  await settle(() => ok(text().includes("Document types aren’t available here"), "an unknown capability is unsupported"), "unsupported");
  ok(text().includes("This workspace uses a newer version of document review than this page supports. Reload the page to update it."), "unsupported says why");
  ok(!text().includes("Default for new types"), "an unsupported board invents no baseline");

  // ---- loading and a failed probe ----

  await fresh({}, false);
  await settle(() => ok(text().includes("Loading document types…"), "no answer yet reads as loading"), "loading");
  ok(status().includes("Loading document types…"), "loading is a polite status");
  ok(!text().includes("Default for new types"), "loading invents no baseline");

  probeReply = () => refusal(500, "internal");
  await React.act(async () => { await documentsProbe.revalidate(); });
  await settle(() => ok(text().includes("Couldn’t load document types."), "a failed probe says so"), "probe error");
  ok(host.querySelector('[role="alert"]'), "a failed probe is an alert");
  ok(!text().includes("Default for new types"), "a failed probe invents no baseline");
  probeReply = () => json(probeBody());
  const probesBefore = gets("/api/document-types");
  await click(buttonIn(host, "Retry"));
  await listed();
  equal(gets("/api/document-types"), probesBefore + 1, "Retry asks the probe again");

  // ---- access still being checked ----

  await fresh({ viewer: CHECKING });
  await settle(() => ok(text().includes("Checking access…"), "unknown access reads as checking"), "checking access");
  ok(!text().includes("The workspace owner manages document types."), "the owner never sees the read-only note while access is checked");
  await fresh({ viewer: UNCONFIRMED });
  await settle(() => ok(text().includes("Access could not be confirmed"), "an unanswered access check says so"), "access unavailable");
  await click(buttonIn(host, "Retry access check"));
  equal(accessRetries, 1, "Retry access check asks the board again");

  // ---- the baseline only ----

  await fresh();
  probeReply = () => json(probeBody([]));
  await React.act(async () => { await documentsProbe.refresh(); });
  await settle(() => ok(text().includes("No type has its own preferences yet"), "no types"), "baseline only");
  const baseline = host.querySelector("[data-baseline]");
  ok(text().includes("Default for new types"), "the baseline is the default for new types");
  equal(
    [value(baseline, "Approval"), value(baseline, "Approver"), value(baseline, "Archive after"), value(baseline, "Recipient note")],
    ["Required before sharing", "Workspace owner", "30 days after delivery", "None"],
    "the baseline's values, as the board sent them",
  );
  equal(rowLabels(), [], "no type rows");

  // ---- pending and confirmed rows, for the owner ----

  await fresh();
  await listed();
  equal(rowLabels(), ["brief", "contract", "memo"], "every type, in the board's order");
  await settle(() => equal(value(row("brief"), "Approver"), "Ann Lee", "the approver's name comes from the roster"), "roster names");
  equal(gets("/api/team/members"), 1, "the roster is read once");
  equal(
    [value(row("brief"), "Approval"), value(row("brief"), "Archive after"), value(row("brief"), "Recipient note")],
    ["Required before sharing", "14 days after delivery", NOTE],
    "a confirmed type's values",
  );
  // The note is plain text: no element is made from it.
  ok(!row("brief")!.querySelector("[data-recipient-note] *"), "the recipient note makes no element");
  ok(!row("brief")!.querySelector("b + b, strong"), "markup in a note is never rendered");
  equal(row("brief")!.querySelector("[data-recipient-note]")?.innerHTML.includes("&lt;b&gt;"), true, "the note's angle brackets are escaped");
  equal(
    [value(row("contract"), "Approval"), value(row("contract"), "Approver"), value(row("contract"), "Archive after")],
    ["Not required", "Workspace owner", "90 days after delivery"],
    "a pending type's values",
  );
  equal(value(row("memo"), "Archive after"), "1 day after delivery", "one day reads in the singular");
  ok(row("contract")!.textContent?.includes("Needs your confirmation"), "a pending row asks the owner to confirm");
  ok(!row("brief")!.textContent?.includes("Needs your confirmation"), "a confirmed row has no badge");
  equal(value(row("memo"), "Approver"), "No longer a member", "an approver the roster lacks");
  ok(row("memo")!.textContent?.includes("This approver is no longer a member, so nobody can approve this type."), "the owner is told the approver left");
  ok(!row("brief")!.textContent?.includes("nobody can approve"), "a present approver is not flagged");
  ok(text().includes("Default for new types"), "the baseline shows beside the types");
  // The owner's controls.
  ok(buttonIn(host, "Add type"), "the owner can add a type");
  ok(buttonIn(row("contract"), "Confirm"), "a pending row has Confirm");
  ok(!buttonIn(row("brief"), "Confirm") && !buttonIn(row("memo"), "Confirm"), "a confirmed row has no Confirm");
  for (const label of ["brief", "contract", "memo"]) ok(buttonIn(row(label), "Edit"), `the owner can edit ${label}`);
  equal(buttonIn(row("contract"), "Confirm")?.getAttribute("aria-label"), "Confirm contract", "Confirm names its type");
  equal(buttonIn(row("brief"), "Edit")?.getAttribute("aria-label"), "Edit brief", "Edit names its type");
  ok(!buttonIn(row("contract"), "Confirm")!.className.includes("btn-primary"), "a row's Confirm is not the page's primary action");
  // Tab walks the page's actions in reading order, through each row's actions.
  const tabbable = () =>
    Array.from(host.querySelectorAll<HTMLElement>("button, a[href], input, select, textarea, [tabindex]"))
      .filter((element) => element.tabIndex >= 0 && !(element as HTMLButtonElement).disabled)
      .map((element) => element.getAttribute("aria-label") ?? element.textContent?.trim());
  equal(tabbable(), ["Add type", "Edit brief", "Confirm contract", "Edit contract", "Edit memo"], "the Tab order through the rows' actions");
  ok(!text().includes("The workspace owner manages document types."), "the owner has no read-only note");

  // A name falls back to the email.
  probeReply = () => json(probeBody([{ ...BRIEF, approver: "u-ben" }]));
  await React.act(async () => { await documentsProbe.refresh(); });
  await settle(() => equal(value(row("brief"), "Approver"), "ben@example.com", "a nameless member shows their email"), "email fallback");

  // ---- read-only viewers ----

  for (const [who, viewer, sub, note] of [
    ["a member", MEMBER, "u-ann", "The workspace owner manages document types."],
    ["a read-only board", READ_ONLY_OWNER, "u-owner", "This board is read-only, so document types can’t be changed here."],
    ["an operator session", OWNER, "operator-token:ops", "The workspace owner manages document types."],
  ] as const) {
    const operator = sub.startsWith("operator-token:");
    await fresh({ viewer, sub }, false);
    // The real roster refuses a platform session: it has no membership row.
    if (operator) rosterReply = () => refusal(403, "membership_changed");
    await React.act(async () => { await documentsProbe.revalidate(); });
    await listed();
    ok(text().includes(note), `${who} is told why the page is read-only`);
    for (const other of ["The workspace owner manages document types.", "This board is read-only, so document types can’t be changed here."])
      if (other !== note) ok(!text().includes(other), `${who} is not told “${other}”`);
    equal(host.querySelectorAll("li[data-type-label] button").length, 0, `${who} gets no row control`);
    ok(!buttonIn(host, "Add type"), `${who} cannot add a type`);
    ok(row("contract")!.textContent?.includes("Waiting for the owner"), `${who} sees the pending row as waiting for the owner`);
    ok(!text().includes("Needs your confirmation"), `${who} is not asked to confirm`);
    ok(!text().includes("nobody can approve this type"), `${who} is not told to update the type`);
    if (!operator) {
      await settle(() => equal(value(row("brief"), "Approver"), "Ann Lee", `${who} reads approver names`), `${who} names`);
      continue;
    }
    // A platform session never asks the roster, so it never fails: approvers
    // show as the owner role or their id, with a quiet note and no retry.
    for (let i = 0; i < 5; i++) await flush();
    equal(gets("/api/team/members"), 0, "an operator session does not read the roster");
    equal(
      [value(row("brief"), "Approver"), value(row("contract"), "Approver"), value(row("memo"), "Approver")],
      ["u-ann", "Workspace owner", "u-gone"],
      "an operator session sees the owner role or the approver's id",
    );
    ok(text().includes("Member names aren’t shown to platform sessions."), "an operator session is told why names are missing");
    ok(!text().includes("Couldn’t read the member list") && !text().includes("Couldn’t refresh the member list"), "an operator session sees no roster failure");
    ok(!buttonIn(host, "Retry"), "an operator session gets no roster retry");
  }

  // ---- confirming a pending type ----

  await fresh();
  await listed();
  await click(buttonIn(row("contract"), "Confirm"));
  ok(dialog(), "Confirm opens a dialog");
  ok(dialog()!.textContent?.includes("Confirm “contract”? These preferences then apply to every document of this type."), "the dialog names the type");
  equal(value(dialog(), "Archive after"), "90 days after delivery", "the dialog lists the values being confirmed");
  equal(posts().length, 0, "opening the dialog writes nothing");
  // Escape closes it and returns focus to the trigger.
  await React.act(async () => {
    dialog()!.dispatchEvent(new win.KeyboardEvent("keydown", { key: "Escape", bubbles: true, cancelable: true }));
  });
  await flush();
  ok(!dialog(), "Escape closes the dialog");
  focused(buttonIn(row("contract"), "Confirm"), "focus returns to Confirm");

  writeReply = (path) =>
    path === "/api/document-types/contract/confirm" ? json({ type: { ...CONTRACT, state: "confirmed", rev: 2 } }) : refusal(500, "internal");
  probeReply = () => json(probeBody([BRIEF, { ...CONTRACT, state: "confirmed", rev: 2 }, MEMO]));
  // A row-like element outside the page must never take the focus.
  const decoy = document.createElement("div");
  decoy.setAttribute("data-record-id", "contract");
  decoy.innerHTML = '<span class="document-type-actions"><button type="button">decoy</button></span>';
  document.body.prepend(decoy);
  await click(buttonIn(row("contract"), "Confirm"));
  ok(buttonIn(dialog(), "Confirm")!.className.includes("btn-primary"), "the dialog's Confirm is primary");
  const probesBeforeConfirm = gets("/api/document-types");
  const rosterBeforeConfirm = gets("/api/team/members");
  await click(buttonIn(dialog(), "Confirm"));
  await settle(() => ok(status().includes("Document type confirmed."), "confirming says so"), "confirmed");
  equal(posts().map((call) => [call.path, call.body]), [["/api/document-types/contract/confirm", { rev: 1 }]], "Confirm sends exactly the row's rev");
  equal(posts()[0]!.headers["X-Cadence-Board"], "1", "a board write");
  ok(!row("contract")!.textContent?.includes("Needs your confirmation"), "the confirmed row has no badge");
  ok(!buttonIn(row("contract"), "Confirm"), "the confirmed row has no Confirm");
  await settle(() => equal(gets("/api/document-types"), probesBeforeConfirm + 1, "the types are read again"), "reread after confirm");
  await settle(() => focused(buttonIn(row("contract"), "Edit"), "focus lands on the row's Edit"), "focus after confirm");
  decoy.remove();
  await settle(() => equal(gets("/api/team/members"), rosterBeforeConfirm + 1, "a write reads the roster again"), "roster after confirm");

  // A roster that fails to refresh keeps the last good one, and says so.
  await fresh();
  await listed();
  await settle(() => equal(value(row("brief"), "Approver"), "Ann Lee", "names"), "roster before refresh");
  rosterReply = () => refusal(500, "internal");
  writeReply = () => json({ type: { ...CONTRACT, state: "confirmed", rev: 2 } });
  await click(buttonIn(row("contract"), "Confirm"));
  await click(buttonIn(dialog(), "Confirm"));
  await settle(() => ok(text().includes("Couldn’t refresh the member list, so approver names may be out of date."), "a failed refresh says so"), "roster refresh failed");
  equal(value(row("brief"), "Approver"), "Ann Lee", "the last good roster still names the approver");
  ok(!text().includes("approvers are shown by their id"), "a kept roster is not described as ids");
  await click(buttonIn(host, "Retry"));
  await settle(() => ok(text().includes("Couldn’t refresh the member list"), "a failed retry still says so"), "retry fails");
  equal(value(row("brief"), "Approver"), "Ann Lee", "a failed retry keeps the last good roster");
  rosterReply = () => json({ members: ROSTER });
  await click(buttonIn(host, "Retry"));
  await settle(() => ok(!text().includes("Couldn’t refresh the member list"), "a good retry clears the notice"), "retry ok");

  // `approver_unavailable` proves the kept roster is out of date. If reading it
  // again fails, the old roster is dropped and approvers show by id, so one who
  // has left is never shown as a member.
  for (const via of ["confirm", "save"] as const) {
    await fresh();
    await listed();
    await settle(() => equal(value(row("brief"), "Approver"), "Ann Lee", "names"), `${via}: roster before`);
    writeReply = () => refusal(409, "approver_unavailable");
    rosterReply = () => refusal(500, "internal");
    const rosterBefore = gets("/api/team/members");
    if (via === "confirm") {
      await click(buttonIn(row("contract"), "Confirm"));
      await click(buttonIn(dialog(), "Confirm"));
    } else {
      await click(buttonIn(row("brief"), "Edit"));
      await fill(field("Archive after (days)"), "15");
      await save();
    }
    await settle(() => equal(gets("/api/team/members"), rosterBefore + 1, `${via}: the roster is read again`), `${via}: roster reread`);
    await settle(
      () => ok(text().includes("Couldn’t read the member list, so approvers are shown by their id."), `${via}: the dropped roster says so`),
      `${via}: roster dropped`,
    );
    equal(value(row("brief"), "Approver"), "u-ann", `${via}: the approver shows by id, not from the stale roster`);
    equal(value(row("memo"), "Approver"), "u-gone", `${via}: an unknown roster never says the approver left`);
    ok(!text().includes("approver names may be out of date"), `${via}: a dropped roster is not described as kept`);
  }

  // A conflict reads the types again and shows the current row.
  const confirmRefusals: [number, string, string][] = [
    [409, "conflict", "This type changed since you read it. Review the current values, then confirm."],
    [404, "not_found", "This type no longer exists."],
    [409, "approver_unavailable", "This type’s approver is no longer a member. Edit the type to choose another approver."],
    [503, "capability_unavailable", "The workspace’s member list couldn’t be read just now, so nothing was changed. Try again later."],
    [403, "owner_only", "Only the workspace owner can change document types."],
    [403, "owner_required", "Only the workspace’s own members can change document types."],
    [403, "membership_changed", "Your access changed. Reload the page and try again."],
    [500, "internal", "Couldn’t confirm this type. Try again."],
    [418, "something_new", "Couldn’t confirm this type. Try again."],
  ];
  for (const [statusCode, code, copy] of confirmRefusals) {
    await fresh();
    await listed();
    writeReply = () => refusal(statusCode, code);
    // What the board holds now: the conflict moved the row on, the 404 removed it.
    const current =
      code === "conflict"
        ? [BRIEF, { ...CONTRACT, archiveAfterDays: 60, rev: 2 }, MEMO]
        : code === "not_found"
          ? [BRIEF, MEMO]
          : [BRIEF, CONTRACT, MEMO];
    probeReply = () => json(probeBody(current));
    const before = gets("/api/document-types");
    const rosterBefore = gets("/api/team/members");
    await click(buttonIn(row("contract"), "Confirm"));
    await click(buttonIn(dialog(), "Confirm"));
    await settle(() => ok(status().includes(copy), `${code} reads “${copy}”`), `confirm ${code}`);
    ok(!status().includes("server says"), `${code} never shows the server's message`);
    ok(!dialog(), `${code} closes the dialog`);
    await settle(() => equal(gets("/api/document-types"), before + 1, `${code} reads the types again`), `${code} reread`);
    if (code === "conflict")
      await settle(() => equal(value(row("contract"), "Archive after"), "60 days after delivery", "a conflict shows the current row"), "conflict row");
    if (code === "not_found") await settle(() => equal(rowLabels(), ["brief", "memo"], "a missing row leaves the list"), "404 row");
    if (code === "approver_unavailable")
      await settle(() => equal(gets("/api/team/members"), rosterBefore + 1, "approver_unavailable reads the roster again"), "roster reread");
  }

  // An abort may land after the server confirmed: say nothing, read again.
  await fresh();
  await listed();
  writeReply = () => { throw abortError(); };
  const beforeAbort = gets("/api/document-types");
  await click(buttonIn(row("contract"), "Confirm"));
  await click(buttonIn(dialog(), "Confirm"));
  await settle(() => equal(gets("/api/document-types"), beforeAbort + 1, "an aborted confirm reads the types again"), "abort reread");
  ok(!dialog(), "an aborted confirm closes the dialog");
  ok(!status().includes("Couldn’t"), "an aborted confirm reports no failure");

  // ---- adding a type ----

  await fresh();
  await listed();
  await click(buttonIn(host, "Add type"));
  ok(drawer(), "Add type opens the form");
  await settle(() => focused(field("Type name"), "the name field takes focus"), "add focus");
  ok(drawer()!.textContent?.includes("Names are stored in lowercase"), "the form says keys are stored lowercase");
  equal(
    [field("Type name")?.value, checkbox()?.checked, approverTrigger()?.textContent, field("Recipient note")?.value, field("Archive after (days)")?.value],
    ["", true, "Workspace owner", "", "30"],
    "a new type starts from the baseline",
  );
  equal(field("Recipient note")?.getAttribute("type"), "text", "the note is a single-line input");
  // Each check, client-side: nothing is sent.
  await save();
  equal(errorFor("Type name"), "Enter a name for this type.", "an empty name is refused");
  focused(field("Type name"), "focus goes to the first invalid field");
  await fill(field("Type name"), "x".repeat(41));
  equal(errorFor("Type name"), null, "typing clears the field's error");
  await save();
  equal(errorFor("Type name"), "Use 40 characters or fewer.", "a long name is refused");
  await fill(field("Type name"), "Brief");
  await save();
  equal(errorFor("Type name"), "This type already exists. Edit it from the list instead.", "an existing name is refused");
  await fill(field("Type name"), " Proposal ");
  await fill(field("Recipient note"), "tab\there");
  await fill(field("Archive after (days)"), "3651");
  await save();
  equal(errorFor("Recipient note"), "Remove line breaks and other control characters.", "a control character in the note is refused");
  equal(errorFor("Archive after (days)"), "Enter a whole number of days from 1 to 3650.", "3651 days is refused");
  focused(field("Recipient note"), "focus goes to the first invalid field");
  equal(posts().length, 0, "nothing invalid is sent");

  await fill(field("Recipient note"), "Thanks for reading");
  await fill(field("Archive after (days)"), "45");
  await click(checkbox());
  await chooseApprover("Ann Lee");
  equal(approverTrigger()?.textContent, "Ann Lee", "the approver is chosen from the roster");
  const choices = () => Array.from(document.querySelectorAll('[role="option"] .select-label')).map((node) => node.textContent);
  await click(approverTrigger());
  equal(choices(), ["Workspace owner", "Olivia Owner", "Ann Lee", "ben@example.com"], "the approver list is the workspace owner and every member");
  // Escape on the open list closes the list only: the drawer and its draft stay
  // (Browser QA). The drawer's own close waits out a transition, so wait it out.
  await React.act(async () => {
    approverTrigger()!.dispatchEvent(new win.KeyboardEvent("keydown", { key: "Escape", bubbles: true, cancelable: true }));
  });
  await closed();
  ok(!document.querySelector('[role="listbox"]'), "Escape closes the open list");
  ok(drawer(), "Escape on the open list leaves the form open");
  equal(
    [field("Type name")?.value, field("Recipient note")?.value, field("Archive after (days)")?.value, approverTrigger()?.textContent],
    [" Proposal ", "Thanks for reading", "45", "Ann Lee"],
    "Escape on the open list keeps the draft",
  );
  // An Escape something inside already handled (cancelled) never closes the drawer.
  const handled = new win.KeyboardEvent("keydown", { key: "Escape", bubbles: true, cancelable: true });
  handled.preventDefault();
  await React.act(async () => { window.dispatchEvent(handled); });
  await closed();
  ok(drawer(), "a handled Escape leaves the form open");
  writeReply = (path, body) =>
    path === "/api/document-types/Proposal"
      ? json({ type: { label: "proposal", ...(body as object), state: "confirmed", rev: 1 } })
      : refusal(500, "internal");
  probeReply = () => json(probeBody([BRIEF, CONTRACT, MEMO, { label: "proposal", requireApproval: false, approver: "u-ann", recipientNote: "Thanks for reading", archiveAfterDays: 45, state: "confirmed", rev: 1 }]));
  await save();
  await settle(() => ok(status().includes("Document type saved."), "saving says so"), "added");
  equal(
    posts().map((call) => [call.path, call.body, Object.keys(call.body as object)]),
    [["/api/document-types/Proposal", { requireApproval: false, approver: "u-ann", recipientNote: "Thanks for reading", archiveAfterDays: 45 }, ["requireApproval", "approver", "recipientNote", "archiveAfterDays"]]],
    "a new type sends its trimmed name in the path and exactly the four preference fields",
  );
  equal(posts()[0]!.headers["Content-Type"], "application/json", "an exact JSON body");
  ok(!drawer(), "a saved form closes");
  equal(rowLabels(), ["brief", "contract", "memo", "proposal"], "the new type joins the list in order");
  equal(value(row("proposal"), "Approver"), "Ann Lee", "with its values");
  await settle(() => focused(buttonIn(row("proposal"), "Edit"), "focus lands on the new row's Edit"), "focus after add");

  // ---- editing a type ----

  await fresh();
  await listed();
  await settle(() => equal(value(row("brief"), "Approver"), "Ann Lee", "names"), "roster for edit");
  await click(buttonIn(row("brief"), "Edit"));
  ok(drawer(), "Edit opens the form");
  ok(!field("Type name"), "an existing type's name is not editable");
  equal(
    [checkbox()?.checked, approverTrigger()?.textContent, field("Recipient note")?.value, field("Archive after (days)")?.value],
    [true, "Ann Lee", NOTE, "14"],
    "the form starts from the row",
  );
  ok(!buttonIn(drawer(), "Save"), "an unchanged form offers no Save");
  ok(!drawer()!.textContent?.includes("The member list couldn’t be read"), "a readable roster needs no hint");
  // An untouched form follows the row when it is read again.
  probeReply = () => json(probeBody([{ ...BRIEF, archiveAfterDays: 9, rev: 3 }, CONTRACT, MEMO]));
  await React.act(async () => { await documentsProbe.refresh(); });
  await settle(() => equal(field("Archive after (days)")?.value, "9", "an untouched form takes the re-read row"), "reseed");
  ok(!drawer()!.textContent?.includes("This type changed after you opened it."), "an untouched form needs no changed notice");
  ok(!buttonIn(drawer(), "Save"), "a re-seeded form is still unchanged");
  await fill(field("Archive after (days)"), "21");
  await chooseApprover("Workspace owner");
  ok(buttonIn(drawer(), "Save"), "a changed form offers Save");

  // A draft survives a re-read, even when the row moved on underneath it.
  await fill(field("Recipient note"), "Draft note");
  probeReply = () => json(probeBody([{ ...BRIEF, archiveAfterDays: 7, rev: 4 }, CONTRACT, MEMO]));
  await React.act(async () => { await documentsProbe.refresh(); });
  await settle(() => equal(value(row("brief"), "Archive after"), "7 days after delivery", "the list shows the re-read row"), "reread under draft");
  ok(drawer(), "the form stays open across a re-read");
  equal(
    [field("Recipient note")?.value, field("Archive after (days)")?.value, approverTrigger()?.textContent],
    ["Draft note", "21", "Workspace owner"],
    "the draft survives the re-read",
  );
  ok(drawer()!.textContent?.includes("This type changed after you opened it. Saving replaces it with the values below."), "the form says the type changed");

  // Server refusals keep the draft and say why.
  const saveRefusals: [number, string, string, string | null][] = [
    [400, "invalid_request", "The board refused these values. Check each field and try again.", null],
    [409, "approver_unavailable", "That approver is no longer a member. Choose another approver.", "Approver"],
    [503, "capability_unavailable", "The workspace’s member list couldn’t be read just now, so nothing was changed. Try again later.", null],
    [403, "owner_only", "Only the workspace owner can change document types.", null],
    [403, "owner_required", "Only the workspace’s own members can change document types.", null],
    [500, "internal", "Couldn’t save this type. Try again.", null],
  ];
  for (const [statusCode, code, copy, at] of saveRefusals) {
    writeReply = () => refusal(statusCode, code);
    const rosterBefore = gets("/api/team/members");
    await save();
    await settle(() => ok(drawer()?.textContent?.includes(copy), `${code} reads “${copy}”`), `save ${code}`);
    ok(!drawer()!.textContent?.includes("server says"), `${code} never shows the server's message`);
    if (at) equal(errorFor(at), copy, `${code} is shown at the ${at} field`);
    equal(field("Recipient note")?.value, "Draft note", `${code} keeps the draft`);
    if (code === "approver_unavailable")
      await settle(() => equal(gets("/api/team/members"), rosterBefore + 1, "approver_unavailable reads the roster again"), "save roster reread");
    expectSaveFocus(code, copy, at !== null);
  }
  // A save that got no answer at all keeps the drawer too, with focus on its error.
  writeReply = () => { throw new TypeError("network down"); };
  await save();
  await settle(() => ok(drawer()?.textContent?.includes("Couldn’t save this type. Try again."), "a failed save says so"), "save failed");
  expectSaveFocus("a failed save", "Couldn’t save this type. Try again.", false);
  const editPosts = posts();
  equal(editPosts[0]!.path, "/api/document-types/brief", "an edit writes the row's own key");
  equal(editPosts[0]!.body, { requireApproval: true, approver: null, recipientNote: "Draft note", archiveAfterDays: 21 }, "an edit sends exactly the four fields, the owner role as null");

  writeReply = (_path, body) => json({ type: { ...BRIEF, ...(body as object), rev: 5 } });
  probeReply = () => json(probeBody([{ ...BRIEF, approver: null, recipientNote: "Draft note", archiveAfterDays: 21, rev: 5 }, CONTRACT, MEMO]));
  await save();
  await settle(() => ok(status().includes("Document type saved."), "an edit saves"), "edited");
  ok(!drawer(), "a saved edit closes");
  equal(value(row("brief"), "Recipient note"), "Draft note", "the row shows the saved values");

  // An aborted save may have applied too.
  await click(buttonIn(row("brief"), "Edit"));
  await fill(field("Archive after (days)"), "23");
  writeReply = () => { throw abortError(); };
  const beforeAbortSave = gets("/api/document-types");
  await save();
  await settle(() => equal(gets("/api/document-types"), beforeAbortSave + 1, "an aborted save reads the types again"), "abort save reread");
  ok(!drawer(), "an aborted save closes the form");
  ok(!status().includes("Couldn’t"), "an aborted save reports no failure");

  // A save whose answer is unreadable may have applied: the list is read again.
  await click(buttonIn(row("brief"), "Edit"));
  await fill(field("Archive after (days)"), "22");
  writeReply = () => new Response("not json", { status: 200 });
  const beforeUnsettled = gets("/api/document-types");
  await save();
  await settle(() => ok(status().includes("Couldn’t confirm the save, so the list was read again."), "an unreadable receipt says so"), "unsettled");
  ok(!drawer(), "an unsettled save closes the form");
  await settle(() => equal(gets("/api/document-types"), beforeUnsettled + 1, "an unsettled save reads the types again"), "unsettled reread");

  // Escape closes the form and returns focus to its Edit.
  await click(buttonIn(row("brief"), "Edit"));
  ok(drawer(), "the form is open");
  await React.act(async () => {
    window.dispatchEvent(new win.KeyboardEvent("keydown", { key: "Escape", bubbles: true, cancelable: true }));
  });
  await closed();
  await settle(() => ok(!drawer(), "Escape closes the form"), "escape form");
  await settle(() => focused(buttonIn(row("brief"), "Edit"), "focus returns to Edit"), "escape focus");

  // A save that answers after its drawer closed never closes the drawer open now.
  let releaseSave: (response: Response) => void = () => {};
  writeReply = () => new Promise<Response>((resolve) => { releaseSave = resolve; });
  await click(buttonIn(row("brief"), "Edit"));
  await fill(field("Archive after (days)"), "33");
  await save();
  await React.act(async () => {
    window.dispatchEvent(new win.KeyboardEvent("keydown", { key: "Escape", bubbles: true, cancelable: true }));
  });
  await closed();
  await settle(() => ok(!drawer(), "Escape closes the saving form"), "escape saving");
  await click(buttonIn(row("contract"), "Edit"));
  await fill(field("Recipient note"), "Second draft");
  const lateRow = { ...BRIEF, approver: null, recipientNote: "Draft note", archiveAfterDays: 33, rev: 6 };
  probeReply = () => json(probeBody([lateRow, CONTRACT, MEMO]));
  await React.act(async () => { releaseSave(json({ type: lateRow })); });
  await settle(() => equal(value(row("brief"), "Archive after"), "33 days after delivery", "the late receipt still applies"), "late receipt");
  ok(drawer(), "the drawer open now stays open");
  equal(drawer()!.getAttribute("aria-label"), "contract", "it is still the second row's drawer");
  equal(field("Recipient note")?.value, "Second draft", "its draft is kept");
  ok(drawer()!.contains(document.activeElement), "focus stays in the open drawer");
  await click(drawer()!.querySelector('button[aria-label^="Close"]'));
  await closed();
  await settle(() => ok(!drawer(), "the second form closes"), "close second");

  // The same race with the newer opening asked for in the same turn as the
  // receipt: the opening is recorded when it is asked for, so the receipt never
  // closes it. This pins the behaviour, not the mechanism: run once with the
  // ref synced in an effect instead (CAD-2286 review), it still passed, since a
  // click's commit flushes its effects before the receipt's continuation runs.
  writeReply = () => new Promise<Response>((resolve) => { releaseSave = resolve; });
  await click(buttonIn(row("brief"), "Edit"));
  await fill(field("Archive after (days)"), "34");
  await save();
  const racedRow = { ...lateRow, archiveAfterDays: 34, rev: 7 };
  probeReply = () => json(probeBody([racedRow, CONTRACT, MEMO]));
  await React.act(async () => {
    buttonIn(row("contract"), "Edit")!.click();
    releaseSave(json({ type: racedRow }));
    for (let i = 0; i < 5; i++) await new Promise((resolve) => setTimeout(resolve, 0));
  });
  await flush();
  await settle(() => equal(value(row("brief"), "Archive after"), "34 days after delivery", "the raced receipt still applies"), "raced receipt");
  ok(drawer(), "the drawer opened before the commit stays open");
  equal(drawer()!.getAttribute("aria-label"), "contract", "it is the newer opening's drawer");
  await click(drawer()!.querySelector('button[aria-label^="Close"]'));
  await closed();
  await settle(() => ok(!drawer(), "the raced form closes"), "close raced");

  // An approver who has left stays in the form, marked, rather than changing unseen.
  await click(buttonIn(row("memo"), "Edit"));
  equal(approverTrigger()?.textContent, "u-gone (not a current member)", "a departed approver stays selected and is marked");
  await click(drawer()!.querySelector('button[aria-label^="Close"]'));
  await closed();
  await settle(() => ok(!drawer(), "the form closes"), "close memo");

  // ---- an unreadable roster ----

  for (const [why, reply] of [
    ["a refused roster", () => refusal(500, "internal")],
    ["a 503 capability_unavailable roster", () => refusal(503, "capability_unavailable")],
    ["a malformed roster", () => json({ members: [{ userId: "", role: "member", joinedAt: "x" }] })],
  ] as const) {
    await fresh({}, false);
    rosterReply = reply;
    await React.act(async () => { await documentsProbe.revalidate(); });
    await listed();
    await settle(() => ok(text().includes("Couldn’t read the member list, so approvers are shown by their id."), `${why} says so`), `${why} notice`);
    equal(value(row("brief"), "Approver"), "u-ann", `${why}: an approver shows by id`);
    equal(value(row("memo"), "Approver"), "u-gone", `${why}: an unknown roster never says the approver left`);
    ok(!text().includes("nobody can approve this type"), `${why} is never a missing approver`);
    await click(buttonIn(row("brief"), "Edit"));
    equal(approverTrigger()?.textContent, "u-ann", `${why}: the form keeps the approver by id`);
    ok(drawer()!.textContent?.includes("The member list couldn’t be read, so only the workspace owner and the current approver are offered."), `${why}: the form says why the list is short`);
    await click(approverTrigger());
    equal(choices(), ["Workspace owner", "u-ann"], `${why}: only the owner role and the current approver are offered`);
    await React.act(async () => {
      approverTrigger()!.dispatchEvent(new win.KeyboardEvent("keydown", { key: "Escape", bubbles: true, cancelable: true }));
    });
    await click(drawer()!.querySelector('button[aria-label^="Close"]'));
    await closed();
    await settle(() => ok(!drawer(), "the form closes"), `${why} close`);
  }
  // Retry reads the roster again.
  rosterReply = () => json({ members: ROSTER });
  await click(buttonIn(host, "Retry"));
  await settle(() => equal(value(row("brief"), "Approver"), "Ann Lee", "a retried roster names the approver"), "roster retry");
  ok(!text().includes("Couldn’t read the member list"), "the notice goes once the roster reads");
  ok(heading(), "the page keeps its heading");

  // ---- the lifecycle turned off under an open form ----

  await fresh();
  await listed();
  await click(buttonIn(row("brief"), "Edit"));
  await fill(field("Archive after (days)"), "40");
  writeReply = () => refusal(404, "not_found");
  probeReply = () => refusal(404, "not_found");
  await save();
  await settle(() => ok(text().includes("Document review isn’t available on this workspace any more."), "a save answered not_found says review is gone"), "save 404");
  await settle(() => ok(text().includes("Document types aren’t available here"), "the probe is read again and latches unavailable"), "save 404 latch");
  ok(!drawer(), "the form goes with the lifecycle");
  await settle(() => focused(host.querySelector(".page-state-title"), "focus lands on the unavailable state's title"), "save 404 focus");
  const latched = gets("/api/document-types");
  await React.act(async () => { await documentsProbe.revalidate(); });
  equal(gets("/api/document-types"), latched, "the unavailable answer is latched");

  // ---- badges stay whole (Browser QA) ----

  // The stylesheet is stubbed in this runner, so the rule itself is checked:
  // a pending badge never breaks mid-word ("審查/中").
  const css: string = require("fs").readFileSync(
    require("path").resolve("src/features/documents/documents.css"),
    "utf8",
  );
  const rule = (selector: string) => {
    const at = css.indexOf(`${selector} {`);
    return at < 0 ? "" : css.slice(at, css.indexOf("}", at));
  };
  ok(/white-space:\s*nowrap/.test(rule(".document-type-name .chip")), "a type's badge keeps its words whole");
  ok(/overflow-wrap:\s*normal/.test(rule(".document-type-name .chip")), "a type's badge never breaks inside a word");

  // ---- zh-TW, rendered ----

  // The page and its refusal copy render through t(): a zh-TW viewer reads
  // the translated values, controls and refusal, never the English.
  win.localStorage.setItem("cadence-locale", "zh-TW");
  await fresh();
  await listed();
  await settle(() => equal(value(row("brief"), "核准者"), "Ann Lee", "zh-TW labels the approver"), "zh-TW names");
  equal(value(row("contract"), "封存時間"), "交付後 90 天", "zh-TW reads the archive delay");
  ok(row("contract")!.textContent?.includes("需要你確認"), "zh-TW asks the owner to confirm");
  writeReply = () => refusal(409, "conflict");
  await click(buttonIn(row("contract"), "確認"));
  await click(buttonIn(dialog(), "確認"));
  await settle(
    () => ok(status().includes(translateUiText("This type changed since you read it. Review the current values, then confirm.", "zh-TW")), "zh-TW refusal copy"),
    "zh-TW refusal",
  );
  win.localStorage.removeItem("cadence-locale");

  // ---- zh-TW ----

  const strings = [
    "Document types aren’t available here",
    "This workspace uses a newer version of document review than this page supports. Reload the page to update it.",
    "The board’s access check did not answer, so document types stay closed.",
    "Checking whether this session may manage document types…",
    "Couldn’t load document types.",
    "Loading document types…",
    "Couldn’t read the member list, so approvers are shown by their id.",
    "Document types",
    "No type has its own preferences yet",
    "Every document uses the default below.",
    "Needs your confirmation",
    "Waiting for the owner",
    "This approver is no longer a member, so nobody can approve this type. Edit it to choose another approver.",
    "Confirm {label}",
    "Edit {label}",
    "Default for new types",
    "A type uses these until the owner confirms its own preferences.",
    "Who approves each type of document, what recipients are told and when it’s archived.",
    "Add type",
    "The workspace owner manages document types.",
    "Document type saved.",
    "This board is read-only, so document types can’t be changed here.",
    "Member names aren’t shown to platform sessions.",
    "Couldn’t refresh the member list, so approver names may be out of date.",
    "The member list couldn’t be read, so only the workspace owner and the current approver are offered.",
    "Use a name that isn’t only dots.",
    "Use a name without “/” or “\\”.",
    "Document review isn’t available on this workspace any more.",
    "Archive after",
    "Couldn’t confirm the save, so the list was read again.",
    "Confirm document type?",
    "Confirm “{label}”? These preferences then apply to every document of this type.",
    "Document type confirmed.",
    "Approval",
    "Approver",
    "Recipient note",
    "Required before sharing",
    "Not required",
    "Workspace owner",
    "No longer a member",
    "1 day after delivery",
    "{days} days after delivery",
    "Add document type",
    "This type changed after you opened it. Saving replaces it with the values below.",
    "Type name",
    "Up to 40 characters. Names are stored in lowercase, so “Brief” and “brief” are the same type.",
    "Require approval before a document of this type is shared",
    "Who approves documents of this type.",
    "Shown to recipients as plain text, on one line. Up to 200 characters.",
    "Archive after (days)",
    "A delivered document of this type moves to Archived after this many days, from 1 to 3650.",
    "{id} (not a current member)",
    "Remove line breaks and other control characters.",
    "Enter a name for this type.",
    "Use 40 characters or fewer.",
    "This type already exists. Edit it from the list instead.",
    "Choose an approver from the list.",
    "Use 200 characters or fewer.",
    "Enter a whole number of days from 1 to 3650.",
    "That approver is no longer a member. Choose another approver.",
    ...confirmRefusals.map(([, , copy]) => copy),
    ...saveRefusals.map(([, , copy]) => copy),
  ];
  for (const english of strings) ok(translateUiText(english, "zh-TW") !== english, `zh-TW has “${english}”`);
  // Placeholders survive translation.
  for (const english of ["Confirm {label}", "Edit {label}", "{days} days after delivery", "{id} (not a current member)", "Confirm “{label}”? These preferences then apply to every document of this type."]) {
    const placeholder = english.match(/\{\w+\}/)![0];
    ok(translateUiText(english, "zh-TW").includes(placeholder), `zh-TW keeps ${placeholder} in “${english}”`);
  }

  await React.act(async () => { root.unmount(); });
  console.log("document types checks passed");
}

void main().catch((err: unknown) => {
  setTimeout(() => {
    throw err;
  }, 0);
});
