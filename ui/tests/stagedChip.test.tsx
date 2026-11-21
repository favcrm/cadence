export {};
/**
 * CAD-2364: an answer that names a staged write by id shows a chip with its
 * title, app, kind and live status, and the chip opens the rail's own review
 * card. Checked on the real decisions store, rail and answer renderer:
 *
 *  - the match: a full id, a unique prefix (`…` or `...`), and nothing for an
 *    unknown id, an ambiguous or a too-short prefix;
 *  - the answer: a known id is a chip; an unknown full id stays the copy
 *    token and an ambiguous prefix stays plain code, naming no other write;
 *    a known id in a fenced block stays code;
 *  - the chip opens the rail's review card and sends nothing itself;
 *  - deciding on the card changes the chip's status without a reload;
 *  - written Chinese.
 */
declare function require(name: string): any;

function assert(value: unknown, why: string): asserts value {
  if (!value) throw new Error(`stagedChip: ${why}`);
}

async function main() {
const { Window } = require("happy-dom");
const win = new Window({ url: "http://localhost/" });
for (const name of ["window", "document", "Node", "Element", "HTMLElement", "HTMLInputElement", "HTMLSelectElement", "HTMLTextAreaElement", "SVGElement", "navigator", "MutationObserver", "ResizeObserver", "Event", "MouseEvent", "KeyboardEvent", "location", "history", "sessionStorage"])
  Object.defineProperty(globalThis, name, { value: name === "window" ? win : win[name], configurable: true, writable: true });
for (const name of ["addEventListener", "removeEventListener"])
  Object.defineProperty(globalThis, name, { value: win[name].bind(win), configurable: true });
Object.defineProperty(globalThis, "crypto", { value: require("crypto").webcrypto, configurable: true });
Object.defineProperty(globalThis, "IS_REACT_ACT_ENVIRONMENT", { value: true });
const loader = require("module"), originalRequire = loader.prototype.require;
loader.prototype.require = function (this: unknown, id: string) {
  if (id.endsWith(".css")) return {};
  if (id === "@hugeicons/core-free-icons") return new Proxy({}, { get: () => ({}) });
  if (id === "@hugeicons/react") return { HugeiconsIcon: () => null };
  return originalRequire.apply(this, arguments);
};
const React = require("react") as typeof import("react");
const { createRoot } = require("react-dom/client") as typeof import("react-dom/client");
const NeedsRail = (require("../src/features/home/NeedsRail") as typeof import("../src/features/home/NeedsRail")).default;
const Md = (require("../src/ui/Md") as typeof import("../src/ui/Md")).default;
const { StagedSourceContext } = require("../src/ui/StagedChip") as typeof import("../src/ui/StagedChip");
const { LocaleTextProvider } = require("../src/lib/localeText") as typeof import("../src/lib/localeText");
const { translateUiText } = require("../src/lib/locale") as typeof import("../src/lib/locale");
const { resetTodoLocal } = require("../src/features/home/todoLocal") as typeof import("../src/features/home/todoLocal");
const { homeDecisions } = require("../src/features/decisions/useDecisions") as typeof import("../src/features/decisions/useDecisions");
const { parseStagedRow } = require("../src/features/decisions/stagedWrite") as typeof import("../src/features/decisions/stagedWrite");
const { matchStaged } = require("../src/features/decisions/stagedRef") as typeof import("../src/features/decisions/stagedRef");
type StagedWrite = import("../src/features/decisions/stagedWrite").StagedWrite;

// Ids shaped like the board's: 32 hex digits. Two share the first eight.
const GIA = "c895beb94d2a4f7e8b1c6d3e5f708a19";
const SF = "828c1018a5d24c7e9f3b6a1d0e2c4b7a";
const OTHER = "828c1018ffe04b2a9c3d5e7f1a2b3c4d";
const row = (id: string, title: string, extra: Record<string, unknown> = {}) => ({
  id, tool: "app_call", status: "pending", createdAt: Date.now() - 60_000, decidedAt: null,
  title, app: "Accounting", target: { kind: "contact", label: title, id: null },
  changes: [{ field: "name", label: "Name", kind: "added", after: title }], changes_complete: true,
  ...extra,
});

// ---- 1. the match, directly ----
{
  const rows = [row(GIA, "Add contact Gia"), row(SF, "Add contact San Francisco"), row(OTHER, "Add contact Ohm")]
    .map(parseStagedRow).filter((r): r is StagedWrite => r !== null);
  assert(rows.length === 3, "fixture rows parse");
  const title = (token: string) => matchStaged(token, rows)?.title ?? null;
  assert(title(GIA) === "Add contact Gia", "the full id");
  assert(title(GIA.toUpperCase()) === "Add contact Gia", "the full id, any case");
  assert(title("c895beb9…") === "Add contact Gia", "a unique prefix with an ellipsis");
  assert(title("c895beb9...") === "Add contact Gia", "a unique prefix with three dots");
  assert(title("c895beb94d2a") === "Add contact Gia", "a unique bare prefix");
  assert(title("828c1018a5…") === "Add contact San Francisco", "a longer prefix settles a shared start");
  assert(title("828c1018…") === null, "a prefix two writes share is no one");
  assert(title("c895beb…") === null, "a prefix under eight characters is no one");
  assert(title("deadbeef00112233deadbeef00112233") === null && title("deadbeef…") === null, "an unknown id is no one");
  assert(title(`${GIA}0`) === null, "a longer id is not its prefix");
  assert(title("c895 beb9…") === null && title("") === null, "not an id shape");
}

// ---- a fake board: the staged list and the decide routes ----
let actions: Record<string, unknown>[] = [];
let listCalls = 0;
const posts: string[] = [];
const json = (v: unknown, status = 200) => new Response(JSON.stringify(v), { status, headers: { "Content-Type": "application/json" } });
globalThis.fetch = (async (input: unknown, init?: RequestInit) => {
  const url = new URL(String(input), "http://localhost");
  if (url.pathname === "/api/assist/actions") {
    listCalls++;
    return json({ actions });
  }
  const m = /^\/api\/assist\/actions\/([^/]+)\/(approve|deny)$/.exec(url.pathname);
  if (m && init?.method === "POST") {
    posts.push(url.pathname);
    actions = actions.map((a) => (a.id === m[1] ? { ...a, status: m[2] === "approve" ? "approved" : "denied", decidedAt: Date.now(), result: m[2] === "approve" ? { ok: true } : null } : a));
    return json({ ok: true });
  }
  return json({});
}) as typeof fetch;

const flush = () => React.act(async () => { await new Promise((resolve) => setTimeout(resolve, 0)); });
const tick = () => React.act(async () => { await new Promise((resolve) => setTimeout(resolve, 12)); });
async function settle(check: () => void) {
  for (let i = 0; i < 80; i++) {
    await flush();
    await tick();
    try { check(); return; } catch { /* keep polling */ }
  }
  await flush();
  check();
}
async function click(element: Element | undefined | null) {
  assert(element, "click target exists");
  await React.act(async () => { element.dispatchEvent(new MouseEvent("click", { bubbles: true, detail: 1 } as any)); });
  await flush();
}
const text = (el: Element | null | undefined) => el?.textContent?.replace(/[⁨⁩]/g, "").replace(/\s+/g, " ").trim() ?? "";
const drawerEl = () => document.querySelector('.crm-drawer[data-drawer="staged"]') as HTMLElement | null;

const ANSWER = [
  `Staged: supplier contact Gia. Action \`${GIA}\` is awaiting your decision.`,
  "",
  "Please **deny** action `828c1018a5…` rather than approve it.",
  "",
  "Unknown: `deadbeef00112233deadbeef00112233`. Shared start: `828c1018…`.",
  "",
  "```",
  GIA,
  "```",
].join("\n");

/** The rail and an answer in one tree, the way Home renders them. */
async function render(locale: "en" | "zh-TW" = "en") {
  resetTodoLocal();
  listCalls = 0;
  posts.length = 0;
  const host = document.createElement("div");
  document.body.append(host);
  const root = createRoot(host);
  const overview = { status: "ok", inFlight: false, error: null, asOf: Date.now(), data: { needs_me: [] } } as never;
  await React.act(async () => {
    root.render(
      React.createElement(
        LocaleTextProvider,
        { value: (english: string) => translateUiText(english, locale) },
        React.createElement(
          StagedSourceContext.Provider,
          { value: homeDecisions },
          React.createElement(NeedsRail, {
            overview, readOnly: false, onOpenIssue: () => {}, permissionsHref: "/settings/permissions",
            onAsk: () => {}, onOpenDecision: () => {}, onAskAgent: () => {},
            decisions: true,
          }),
          React.createElement("div", { className: "answer", "data-answer": true }, React.createElement(Md, { text: ANSWER, idTokens: true })),
        ),
      ),
    );
  });
  const answer = () => host.querySelector("[data-answer]") as HTMLElement;
  const chips = () => Array.from(answer().querySelectorAll("[data-staged-chip]")) as HTMLButtonElement[];
  await settle(() => assert(chips().length === 2, `the answer's two known ids are chips (got ${chips().length})`));
  const done = async () => { await React.act(async () => { root.unmount(); }); host.remove(); homeDecisions.resetDecisions(); };
  return { host, answer, chips, done };
}

// ---- 2. known ids are chips; unknown and ambiguous ids stay plain and name no one ----
actions = [row(GIA, "Add contact Gia"), row(SF, "Add contact San Francisco"), row(OTHER, "Add contact Ohm", { app: "CRM" })];
{
  const v = await render();
  const [gia, sf] = v.chips();
  assert(gia.tagName === "BUTTON" && gia.getAttribute("type") === "button", "the chip is a button");
  assert(text(gia.querySelector(".stagedchip-title")) === "Add contact Gia", "the title");
  assert(text(gia.querySelector(".stagedchip-meta")) === "Accounting · Business", `app and kind (got "${text(gia.querySelector(".stagedchip-meta"))}")`);
  assert(text(gia.querySelector(".stagedchip-status")) === "Waiting" && gia.getAttribute("data-outcome") === "pending", "pending status");
  const name = gia.getAttribute("aria-label") ?? "";
  assert(name.includes("Add contact Gia") && name.includes("Waiting"), `the accessible name has the title and status (got "${name}")`);
  assert(gia.getAttribute("aria-disabled") === null && gia.tabIndex >= 0, "a waiting chip is reachable and enabled");
  assert(text(sf.querySelector(".stagedchip-title")) === "Add contact San Francisco", "a unique prefix is the same chip");

  const unknown = v.answer().querySelector("[data-id-token]");
  assert(unknown?.getAttribute("title") === "deadbeef00112233deadbeef00112233", "an unknown full id stays the copy token");
  const plain = Array.from(v.answer().querySelectorAll("code")).map((c) => c.textContent);
  assert(plain.includes("828c1018…"), "a shared prefix stays plain code");
  assert(!text(v.answer()).includes("Ohm") && !text(v.answer()).includes("CRM"), "the answer names no write it did not match");
  const fence = v.answer().querySelector("pre > code");
  assert(fence?.textContent?.trim() === GIA && !fence.closest("pre")!.querySelector("[data-staged-chip]"), "a fenced block without a language stays code, never a chip");
  await v.done();
}

// ---- 3. the chip opens the rail's review card and decides nothing; deciding there updates the chip ----
{
  const v = await render();
  const before = listCalls;
  await click(v.chips()[0]);
  await settle(() => assert(drawerEl() !== null, "the rail's review card opens"));
  assert(text(drawerEl()!.querySelector(".crm-drawer-title")) === "Add contact Gia", "on the write the chip names");
  assert(text(drawerEl()).includes("1 of 3"), "with the rail's queue of what is waiting");
  assert(posts.length === 0 && listCalls === before, "opening sends nothing and reads nothing");

  // Deny on the card: the chip follows without a reload of the page.
  await new Promise((resolve) => setTimeout(resolve, 850));
  await click(Array.from(drawerEl()!.querySelectorAll("button")).find((b) => b.textContent?.trim() === "Deny"));
  await settle(() => assert(posts.length === 1 && posts[0].endsWith(`/${GIA}/deny`), "the card sent the deny"));
  await settle(() => assert(text(v.chips()[0].querySelector(".stagedchip-status")) === "Denied", "the chip now says Denied"));
  const denied = v.chips()[0];
  assert(denied.getAttribute("data-outcome") === "denied" && denied.getAttribute("aria-disabled") === "true", "a decided chip has no card to open");
  assert((denied.getAttribute("aria-label") ?? "").includes("Denied"), "its accessible name says so");

  // A change that reaches the store from elsewhere (another tab approved it) shows too.
  actions = actions.map((a) => (a.id === SF ? { ...a, status: "approved", decidedAt: Date.now(), result: { ok: true } } : a));
  await React.act(async () => { await homeDecisions.reloadDecisions(); });
  await settle(() => assert(text(v.chips()[1].querySelector(".stagedchip-status")) === "Applied", "an approved write that applied says Applied"));
  if (drawerEl()) await click(Array.from(drawerEl()!.querySelectorAll("button")).find((b) => b.textContent?.trim() === "Not now"));
  const sent = posts.length;
  await click(denied);
  assert(posts.length === sent, "a decided chip sends nothing");
  await v.done();
}

// ---- 4. written Chinese ----
actions = [row(GIA, "Add contact Gia"), row(SF, "Add contact San Francisco"), row(OTHER, "Add contact Ohm")];
{
  const v = await render("zh-TW");
  const gia = v.chips()[0];
  assert(text(gia.querySelector(".stagedchip-status")) === "等待中", `zh status (got "${text(gia.querySelector(".stagedchip-status"))}")`);
  assert(text(gia.querySelector(".stagedchip-meta")) === "Accounting · 商務", "zh kind");
  assert(gia.getAttribute("title") === "開啟審查卡", "zh hint");
  await v.done();
}

console.log("staged chip checks passed");
}
void main();
