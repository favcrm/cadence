import {
  buildCustomerProfile,
  consentEntries,
  friendlyError,
  newCustomerId,
  parseTags,
  revisionEntries,
  viewProfile,
} from "../src/features/app-shell/customerProfile";
import { hostActions } from "../src/features/app-shell/hostActions";
import { ApiError } from "../src/lib/api";

export {};
/** CAD-781 CRM Customers: pure helpers, wire grammar, mounted screens. */
declare function require(name: string): any;

function equal(actual: unknown, expected: unknown, why: string): void {
  const a = JSON.stringify(actual);
  const e = JSON.stringify(expected);
  if (a !== e) throw new Error(`${why}: expected ${e}, got ${a}`);
}
function assert(value: unknown, why: string): asserts value {
  if (!value) throw new Error(why);
}
async function rejected(action: () => Promise<unknown>, check: (error: unknown) => boolean, why: string) {
  try {
    await action();
  } catch (error) {
    assert(check(error), why);
    return;
  }
  throw new Error(`${why}: expected a refusal`);
}

async function main() {
// Pure profile shaping mirrors the daemon's bounds without echoing values.
equal(
  buildCustomerProfile({
    displayName: "Amina Diallo",
    email: "amina@example.com",
    phone: "+1 555-0100",
    tags: "vip, newsletter",
    source: "import",
    consentEmail: "granted",
    consentSms: "unknown",
  }),
  {
    schema: 1,
    display_name: "Amina Diallo",
    tags: ["vip", "newsletter"],
    consent: { email: "granted", sms: "unknown" },
    email: "amina@example.com",
    phone: "+1 555-0100",
    source: "import",
  },
  "valid fields build the allowlisted profile",
);
equal(
  buildCustomerProfile({
    displayName: "No Contact",
    email: "",
    phone: "",
    tags: "",
    source: "",
    consentEmail: "unknown",
    consentSms: "unknown",
  }),
  { schema: 1, display_name: "No Contact", tags: [], consent: { email: "unknown", sms: "unknown" } },
  "empty optionals stay absent, consent stays explicit unknown",
);
for (const [fields, why] of [
  [{ displayName: "", email: "", phone: "", tags: "", source: "", consentEmail: "unknown", consentSms: "unknown" }, "blank name"],
  [{ displayName: "  spaced  ", email: "", phone: "", tags: "", source: "", consentEmail: "unknown", consentSms: "unknown" }, "padded name"],
  [{ displayName: "Ok", email: "not-an-email", phone: "", tags: "", source: "", consentEmail: "unknown", consentSms: "unknown" }, "bad email"],
  [{ displayName: "Ok", email: "", phone: "123", tags: "", source: "", consentEmail: "unknown", consentSms: "unknown" }, "short phone"],
  [{ displayName: "Ok", email: "", phone: "", tags: "no spaces allowed!!", source: "", consentEmail: "unknown", consentSms: "unknown" }, "bad tag"],
  [{ displayName: "Ok", email: "", phone: "", tags: "vip,vip", source: "", consentEmail: "unknown", consentSms: "unknown" }, "duplicate tag"],
  [{ displayName: "Ok", email: "", phone: "", tags: "", source: "has space", consentEmail: "unknown", consentSms: "unknown" }, "bad source"],
] as any[]) {
  await rejected(() => Promise.resolve().then(() => buildCustomerProfile(fields)), (e) => e instanceof ApiError, `client refuses ${why} before the wire`);
}
equal(parseTags("vip; newsletter  referral"), ["vip", "newsletter", "referral"], "tag separators split");
await rejected(() => Promise.resolve().then(() => parseTags("a,".repeat(17))), (e) => e instanceof ApiError, "17 tags refused");
assert(/^[A-Za-z0-9_-]{1,128}$/.test(newCustomerId()), "fresh record ids match the peer grammar");

// Defensive reads never throw and never render raw shapes.
equal(viewProfile(null).displayName, "—", "null profile falls back");
equal(viewProfile({ display_name: 7 }).displayName, "—", "non-string name falls back");
equal(revisionEntries({ history: [{ revision: "1" }] } as any), [], "malformed history drops");
equal(consentEntries({ consentHistory: "nope" } as any), [], "malformed consent trail drops");
assert(friendlyError(new ApiError("record revision is stale", 409)).includes("Reload"), "stale errors gain a next step");
assert(!friendlyError(new ApiError("record revision is stale", 409)).includes("@"), "friendly errors echo nothing");

// Wire grammar: selectors ride the peer query string; bounds refuse early.
const seen: string[] = [];
globalThis.fetch = (async (input: unknown, init?: RequestInit) => {
  const path = String(input);
  seen.push(`${init?.method ?? "GET"} ${path}`);
  const url = new URL(path, "http://localhost");
  const query = url.searchParams.get("query") ?? "";
  const limit = Number(url.searchParams.get("limit") ?? "20");
  const cursor = url.searchParams.get("cursor");
  const rows = [
    { id: "customer-s1", revision: 1, profile: { display_name: "Search Alpha One", email: "alpha-one@example.com", tags: ["alpha"], consent: { email: "granted", sms: "unknown" } } },
    { id: "customer-s2", revision: 2, profile: { display_name: "Search Beta Two", email: "beta-two@example.com", tags: ["beta"], source: "import", consent: { email: "denied", sms: "unknown" } } },
    { id: "customer-s3", revision: 3, profile: { display_name: "Search Gamma Three", tags: [], consent: { email: "unknown", sms: "unknown" } } },
  ].filter((r) => !query || JSON.stringify(r).includes(query));
  const start = cursor ? rows.findIndex((r) => r.id === cursor) + 1 : 0;
  const page = rows.slice(start, start + limit);
  const rest = rows.length - start - page.length;
  const body = {
    records: page.map((r) => ({
      ...r,
      install_id: "install-crm",
      context_id: "ctx-a",
      kind: "customer",
      digest: "sha256:abc",
      history: [{ revision: 1, digest: "sha256:abc", actor: "operator", at: 1759113600 }],
      consent_history: [{ revision: 1, channel: "email", state: "granted", actor: "operator", at: 1759113600 }],
    })),
    truncated: rest > 0,
    next_cursor: rest > 0 ? page[page.length - 1].id : null,
  };
  return new Response(JSON.stringify(body), { status: 200 });
}) as typeof fetch;
const scope = { installId: "install-crm", contextId: "ctx-a" };
const first = await hostActions.list(scope, { limit: 2 });
equal(first.records.length, 2, "limit pages the server rows");
equal(first.truncated, true, "truncated signals more");
equal(first.nextCursor, "customer-s2", "continuation cursor is the last id");
const second = await hostActions.list(scope, { limit: 2, cursor: first.nextCursor! });
equal(second.records.map((r) => r.id), ["customer-s3"], "cursor walks without overlap");
const searched = await hostActions.list(scope, { query: "Beta" });
equal(searched.records.map((r) => r.id), ["customer-s2"], "server search narrows rows");
assert(seen.some((s) => s.includes("query=Beta")), "search rides the peer request");
await rejected(() => hostActions.list(scope, { query: "" }), (e) => e instanceof ApiError, "empty query refused");
await rejected(() => hostActions.list(scope, { query: "x".repeat(121) }), (e) => e instanceof ApiError, "overlong query refused");
await rejected(() => hostActions.list(scope, { limit: 0 }), (e) => e instanceof ApiError, "zero limit refused");
await rejected(() => hostActions.list(scope, { limit: 101 }), (e) => e instanceof ApiError, "huge limit refused");
await rejected(() => hostActions.list(scope, { cursor: "../escape" }), (e) => e instanceof ApiError, "cursor escape refused");
// Forged body keys never serialize on create or update.
hostActions.guards.assertCleanBody({ record_id: "r", profile: {} }, ["record_id", "profile"]);
await rejected(
  () => Promise.resolve().then(() => hostActions.guards.assertCleanBody({ expected_revision: 1, profile: {}, actor: "operator" }, ["expected_revision", "profile"])),
  (e) => e instanceof ApiError,
  "forged actor refused client-side",
);

// Mounted CRM outlet: breadcrumb, list, search, new, drawer, placeholders.
const { Window } = require("happy-dom");
const win = new Window({ url: "http://localhost/app-installations/install-crm?ctx=ctx-a" });
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
const CrmOutlet = (require("../src/features/app-shell/CrmOutlet") as typeof import("../src/features/app-shell/CrmOutlet")).default;
const { useHref } = require("../src/lib/useLocation") as typeof import("../src/lib/useLocation");

const posts: { path: string; body: any }[] = [];
const recordStore: Record<string, any> = {
  "customer-s1": { id: "customer-s1", install_id: "install-crm", context_id: "ctx-a", kind: "customer", revision: 1, digest: "sha256:aaa", profile: { schema: 1, display_name: "Search Alpha One", email: "alpha-one@example.com", tags: ["alpha"], consent: { email: "granted", sms: "unknown" } }, history: [{ revision: 1, digest: "sha256:aaa", actor: "operator", at: 1759113600 }], consent_history: [{ revision: 1, channel: "email", state: "granted", actor: "operator", at: 1759113600 }] },
  "customer-s2": { id: "customer-s2", install_id: "install-crm", context_id: "ctx-a", kind: "customer", revision: 2, digest: "sha256:bbb", profile: { schema: 1, display_name: "Search Beta Two", email: "beta-two@example.com", tags: ["beta"], source: "import", consent: { email: "denied", sms: "unknown" } }, history: [{ revision: 1, digest: "sha256:aaa", actor: "operator", at: 1759113600 }, { revision: 2, digest: "sha256:bbb", actor: "operator", at: 1759200000 }], consent_history: [{ revision: 1, channel: "email", state: "granted", actor: "operator", at: 1759113600 }, { revision: 2, channel: "email", state: "denied", actor: "operator", at: 1759200000 }] },
};
globalThis.fetch = (async (input: unknown, init?: RequestInit) => {
  const path = String(input);
  const url = new URL(path, "http://localhost");
  if (init?.body) {
    const body = JSON.parse(String(init.body));
    posts.push({ path, body });
    if (url.pathname.endsWith("/records")) {
      const id = body.record_id;
      if (body.profile?.email === "dupe@example.com" || recordStore[id]) {
        return new Response(JSON.stringify({ error: "record ID already holds different content" }), { status: 409 });
      }
      const record = { id, install_id: "install-crm", context_id: "ctx-a", kind: "customer", revision: 1, digest: "sha256:new", profile: body.profile, history: [{ revision: 1, digest: "sha256:new", actor: "operator", at: 1759286400 }], consent_history: [{ revision: 1, channel: "email", state: body.profile.consent.email, actor: "operator", at: 1759286400 }] };
      recordStore[id] = record;
      return new Response(JSON.stringify({ record }), { status: 200 });
    }
    if (url.pathname.endsWith("/update")) {
      const id = url.pathname.split("/").slice(-2, -1)[0];
      const current = recordStore[id];
      if (!current || body.expected_revision !== current.revision) {
        return new Response(JSON.stringify({ error: "record revision is stale" }), { status: 409 });
      }
      current.revision += 1;
      current.profile = body.profile;
      current.digest = "sha256:upd";
      current.history.push({ revision: current.revision, digest: "sha256:upd", actor: "operator", at: 1759286400 });
      return new Response(JSON.stringify({ record: current }), { status: 200 });
    }
  }
  if (url.pathname.endsWith("/records")) {
    const query = url.searchParams.get("query") ?? "";
    const records = Object.values(recordStore).filter((r: any) => !query || JSON.stringify(r).includes(query));
    return new Response(JSON.stringify({ records, truncated: false, next_cursor: null }), { status: 200 });
  }
  // CAD-784 sections serve empty server rows in this harness — the
  // screens assert their empty states, never placeholders.
  if (url.pathname.endsWith("/segments/list")) {
    return new Response(JSON.stringify({ segments: [] }), { status: 200 });
  }
  if (url.pathname.endsWith("/exclusions/list") || url.pathname.endsWith("/suppressions/list")) {
    const empty = url.pathname.endsWith("/exclusions/list") ? { exclusions: [] } : { suppressions: [] };
    return new Response(JSON.stringify(empty), { status: 200 });
  }
  if (url.pathname.endsWith("/content/campaigns/list")) {
    return new Response(JSON.stringify({ contents: [] }), { status: 200 });
  }
  if (url.pathname.endsWith("/content/proposals/list")) {
    return new Response(JSON.stringify({ proposals: [] }), { status: 200 });
  }
  const id = url.pathname.split("/").pop()!;
  const record = recordStore[id];
  if (!record) return new Response(JSON.stringify({ error: "record is unavailable for this installation and context" }), { status: 409 });
  return new Response(JSON.stringify({ record }), { status: 200 });
}) as typeof fetch;

function Harness({ viewer }: { viewer: { operator: boolean; readOnly: boolean } }) {
  const [view, setView] = React.useState<"list" | "new">("list");
  const [recordId, setRecordId] = React.useState<string | null>(null);
  // CAD-784: the nested CRM section is route state — the harness
  // reads it from the URL like the shell does, so submenu links
  // drive section moves end to end (record view cleared by the
  // link href itself).
  const href = useHref();
  const crm = new URLSearchParams(href.split("?")[1] ?? "").get("crm");
  const section = crm === "segments" || crm === "campaigns" ? crm : "customers";
  React.useEffect(() => {
    const params = new URLSearchParams(href.split("?")[1] ?? "");
    if (recordId) params.set("record", recordId);
    else params.delete("record");
    const s = params.toString();
    history.replaceState(null, "", `/app-installations/install-crm${s ? `?${s}` : ""}`);
  }, [recordId]);
  return React.createElement(CrmOutlet, {
    scope, installationTitle: "CRM", appKind: "crm", view, recordId, section, viewer,
    onView: setView, onSelect: setRecordId,
  });
}

const host = document.createElement("div");
document.body.append(host);
const root = createRoot(host);
const flush = () => React.act(async () => { await new Promise((resolve) => setTimeout(resolve, 0)); });
const text = () => host.textContent ?? "";
async function settle(check: () => void) {
  for (let i = 0; i < 40; i++) {
    await flush();
    try { check(); return; } catch { /* keep polling */ }
  }
  await flush();
  check();
}
async function click(element: Element | undefined | null) {
  assert(element, "click target exists");
  await React.act(async () => { element.dispatchEvent(new MouseEvent("click", { bubbles: true })); });
  await flush();
}
const sleep = (ms: number) => new Promise((resolve) => setTimeout(resolve, ms));
async function fill(selector: string, value: string) {
  const element = host.querySelector(selector) as HTMLInputElement;
  assert(element, `field exists: ${selector}`);
  await React.act(async () => {
    Object.getOwnPropertyDescriptor(HTMLInputElement.prototype, "value")!.set!.call(element, value);
    element.dispatchEvent(new Event("input", { bubbles: true }));
  });
  await flush();
}
// Textareas ride their own prototype setter — fill() above is inputs only.
async function fillArea(selector: string, value: string) {
  const element = host.querySelector(selector) as HTMLTextAreaElement;
  assert(element, `area exists: ${selector}`);
  await React.act(async () => {
    Object.getOwnPropertyDescriptor(HTMLTextAreaElement.prototype, "value")!.set!.call(element, value);
    element.dispatchEvent(new Event("input", { bubbles: true }));
  });
  await flush();
}

await React.act(async () => {
  root.render(React.createElement(Harness, { viewer: { operator: true, readOnly: false } }));
});
await settle(() => assert(text().includes("Search Alpha One"), "list paints real server rows"));
assert(text().includes("Apps") && text().includes("CRM") && text().includes("Customers"), "nested breadcrumb Apps → CRM → Customers");
assert(text().includes("r1") && text().includes("granted"), "revision and consent cues render");
assert(!text().includes("No records yet"), "populated list shows no empty state");

// Search narrows through the peer; the route URL keeps scope only.
await fill("#crm-customer-search", "Beta");
await React.act(async () => { await sleep(400); });
await settle(() => assert(text().includes("Search Beta Two") && !text().includes("Search Alpha One"), "search narrows rows"));
assert(!location.search.includes("Beta"), "search text never enters the URL");
await fill("#crm-customer-search", "");
await React.act(async () => { await sleep(400); });
await settle(() => assert(text().includes("Search Alpha One"), "clearing restores rows"));

// CAD-784 sections render real server-driven screens: empty states off
// empty server rows, and the list never contains an inline builder.
// Moves ride the host-owned submenu links (real anchors), not pane buttons.
const sectionLink = (label: string) =>
  Array.from(host.querySelectorAll('nav[aria-label="CRM sections"] a')).find(
    (el) => (el.textContent ?? "").trim() === label,
  );
assert(sectionLink("Segments")?.tagName === "A", "submenu offers real links");
assert(
  (sectionLink("Segments") as HTMLAnchorElement).getAttribute("href")?.includes("ctx=ctx-a"),
  "submenu links keep the selected context",
);
await click(sectionLink("Segments"));
await settle(() => assert(text().includes("No segments yet in this context"), "segments empty state is server-driven"));
assert(location.search.includes("crm=segments"), "submenu move routes");
await click(sectionLink("Campaigns"));
await settle(() => assert(text().includes("No campaigns yet in this context"), "campaigns empty state is server-driven"));
assert(!host.querySelector('section[aria-label="Campaigns list"] input'), "campaigns list holds no inline builder");
await click(sectionLink("Customers"));
await settle(() => assert(text().includes("Search Alpha One"), "customers link restores the list"));
assert(!location.search.includes("crm="), "customers is the default route");

// Drawer: profile, consent trail, keyboard close with focus restoration.
const openBeta = Array.from(host.querySelectorAll("button.lnk")).find((el) => el.textContent === "Open" && el.closest("tr")?.textContent?.includes("Beta"));
await click(openBeta);
await settle(() => assert(host.querySelector('[data-drawer="customer"]'), "drawer opens"));
assert(text().includes("beta-two@example.com"), "drawer shows the profile");
assert(text().includes("Consent history"), "drawer shows the consent trail");
assert(text().includes("Revision history"), "drawer shows revision history");
assert(text().includes("chat context only"), "drawer names the selection contract");
assert(location.search.includes("record=customer-s2"), "direct record route deep-links");
assert(!location.search.includes("beta-two"), "no customer content in the URL");
await React.act(async () => {
  document.dispatchEvent(new KeyboardEvent("keydown", { key: "Escape", bubbles: true }));
});
await flush();
assert(!host.querySelector('[data-drawer="customer"]'), "Escape closes the drawer");
assert(!location.search.includes("record="), "closing clears the record scope");

// New customer: invalid email blocks before the wire; valid posts exact grammar.
await click(Array.from(host.querySelectorAll("button")).find((el) => el.textContent === "New customer"));
await settle(() => assert(host.querySelector("#crm-display-name"), "new page opens with focusable heading"));
assert(document.activeElement?.id === "crm-display-name" || text().includes("New customer"), "new route renders");
posts.length = 0;
await fill("#crm-display-name", "New Person");
await fill("#crm-email", "not-an-email");
await click(host.querySelector('button[type="submit"]'));
await flush();
assert(posts.length === 0, "invalid email never reaches the wire");
await fill("#crm-email", "new-person@example.com");
await click(host.querySelector('button[type="submit"]'));
await settle(() => assert(posts.length === 1, "valid form posts once"));
equal(Object.keys(posts[0].body).sort(), ["profile", "record_id"], "create body grammar is exact");
equal(posts[0].body.profile.consent, { email: "unknown", sms: "unknown" }, "consent ships explicit unknown, never inferred");
await settle(() => assert(host.querySelector('[data-drawer="customer"]'), "created record opens its drawer"));

// Edit with a stale revision surfaces the server refusal and keeps the drawer.
await click(Array.from(host.querySelectorAll("button")).find((el) => el.textContent === "Edit profile"));
await settle(() => assert(host.querySelector("#crm-display-name"), "edit form opens"));
(recordStore[posts[0].body.record_id] as any).revision = 99;
await fill("#crm-display-name", "New Person Edited");
await click(host.querySelector('[data-drawer="customer"] button[type="submit"]'));
await settle(() => assert(text().includes("stale"), "stale write explains itself with a next step"));

// ---- CAD-865: the CSV import flow — preview, per-row decisions,
// commit. The fixture answers the two reserved bulk routes with the
// daemon's shapes; nothing mutates until the import POST lands.
const csvPosts: { path: string; body: any }[] = [];
const importedIds: string[] = [];
const csvText =
  "record_id,display_name,email,tags,consent_email,expected_revision\n" +
  "customer-9,Chidi Anagonye,chidi@example.com,newcomer,granted,\n" +
  "customer-s2,Boris Feld Jr,beta-two@example.com,,denied,\n" +
  "customer-bad,Bad Row,not-an-email,,granted,\n";
const priorFetch = globalThis.fetch;
globalThis.fetch = (async (input: unknown, init?: RequestInit) => {
  const url = new URL(String(input), "http://localhost");
  if (url.pathname.endsWith("/records/csv-preview")) {
    const body = JSON.parse(String(init!.body));
    csvPosts.push({ path: url.pathname, body });
    return new Response(JSON.stringify({
      preview_token: "sha256:" + "a".repeat(64),
      row_count: 3,
      summary: { create: 1, update: 0, skip: 0, needs_revision: 1, error: 1 },
      rows: [
        { row: 1, record_id: "customer-9", decision: "create", expected_revision: null, current_revision: null, profile: { schema: 1, display_name: "Chidi Anagonye", email: "chidi@example.com", tags: ["newcomer"], consent: { email: "granted" } }, errors: [], reason: null },
        { row: 2, record_id: "customer-s2", decision: "needs_revision", expected_revision: null, current_revision: 2, profile: { schema: 1, display_name: "Boris Feld Jr", email: "beta-two@example.com", tags: [], consent: { email: "denied" } }, errors: [], reason: "missing expected revision" },
        { row: 3, record_id: "customer-bad", decision: "error", expected_revision: null, current_revision: null, profile: null, errors: ["invalid email"], reason: null },
      ],
    }), { status: 200, headers: { "Content-Type": "application/json" } });
  }
  if (url.pathname.endsWith("/records/csv-import")) {
    const body = JSON.parse(String(init!.body));
    csvPosts.push({ path: url.pathname, body });
    // The fixture refuses a token that does not bind the bytes —
    // the same guard the daemon enforces on a stale preview.
    if (body.preview_token !== "sha256:" + "a".repeat(64)) {
      return new Response(JSON.stringify({ error: "customer CSV preview is stale; preview again" }), { status: 409 });
    }
    const decisions: any[] = body.decisions ?? [];
    for (const decision of decisions) {
      if (decision.action !== "skip") importedIds.push(`row-${decision.row}`);
    }
    return new Response(JSON.stringify({
      request_id: body.request_id,
      preview_token: body.preview_token,
      replayed: false,
      summary: { applied: 2, skipped: 1, failed: 0 },
      rows: [
        { row: 1, record_id: "customer-9", outcome: "created", reason: null },
        { row: 2, record_id: "customer-s2", outcome: "updated", reason: null },
        { row: 3, record_id: "customer-bad", outcome: "skipped", reason: "row error" },
      ],
    }), { status: 200, headers: { "Content-Type": "application/json" } });
  }
  return priorFetch(input as RequestInfo | URL, init);
}) as typeof fetch;

// The list's toolbar carries the import affordance next to New.
const importButton = Array.from(host.querySelectorAll("button")).find((el) => el.textContent === "Import CSV");
assert(importButton, "the customers list offers Import CSV");
await click(importButton);
await settle(() => assert(host.querySelector("#csv-text"), "the import page renders its CSV source"));
assert(!location.search.includes("csv"), "no CSV bytes or marker enter the URL");

// Paste the CSV and preview: the plan writes nothing and binds the
// bytes under one token.
await fillArea("#csv-text", csvText);
csvPosts.length = 0;
await click(host.querySelector('form[aria-label="CSV source"] button[type="submit"]'));
await settle(() => assert(host.querySelector("[data-preview-summary]"), "the preview summary renders"));
equal(csvPosts.length, 1, "exactly one preview POST left the browser");
equal(Object.keys(csvPosts[0].body).sort(), ["csv_text"], "preview body is exactly csv_text");
assert(text().includes("1 create"), "preview counts the planned create");
assert(text().includes("1 need a revision"), "preview counts needs_revision rows");
assert(text().includes("1 refused"), "preview counts error rows");
assert(host.querySelector('[data-plan-row="2"] input#csv-revision-2'), "the needs_revision row offers its revision field");
assert(!host.querySelector('[data-plan-row="3"] input#csv-revision-3'), "the error row carries no revision field");

// The apply decision on the needs_revision row needs its confirmed
// revision before the commit arms. The field pre-fills with the row's
// observed current revision; clearing it disarms the commit.
const applyButton = () => Array.from(host.querySelectorAll("button")).find((el) => (el.textContent ?? "").startsWith("Import "));
assert(applyButton() && !(applyButton() as HTMLButtonElement).disabled, "the pre-filled observed revision arms commit");
await fill("#csv-revision-2", "");
assert(applyButton() && (applyButton() as HTMLButtonElement).disabled, "clearing the revision disarms commit");
await fill("#csv-revision-2", "2");
assert(applyButton() && !(applyButton() as HTMLButtonElement).disabled, "the confirmed revision re-arms commit");

// Commit: the import body carries the exact grammar — same bytes,
// the bound token, one request id, explicit per-row decisions.
csvPosts.length = 0;
await click(applyButton());
await settle(() => assert(host.querySelector("[data-import-receipt]"), "the import receipt renders"));
equal(csvPosts.length, 1, "exactly one import POST left the browser");
equal(
  Object.keys(csvPosts[0].body).sort(),
  ["csv_text", "decisions", "preview_token", "request_id"],
  "import body is exactly the allowlist",
);
equal(csvPosts[0].body.preview_token, "sha256:" + "a".repeat(64), "the bound token ships");
assert(/^csv-[0-9a-f]{24}$/.test(csvPosts[0].body.request_id), "the request id is identifier-safe");
const sent = csvPosts[0].body.decisions as any[];
equal(sent.length, 3, "every planned row carries a decision");
equal(
  sent.find((d) => d.row === 2),
  { row: 2, action: "update", expected_revision: 2 },
  "the needs_revision row commits with its confirmed revision",
);
equal(sent.find((d) => d.row === 3)?.action, "skip", "the error row stays skipped");
assert(importedIds.includes("row-1") && importedIds.includes("row-2"), "the committed rows report applied");
assert(text().includes("2 applied"), "the receipt counts applied rows");
assert(text().includes("row error"), "the skipped row's reason stays visible");

// The receipt stays mounted — the operator chooses to return.
assert(host.querySelector('[data-outcome-row="3"]'), "the skipped row's outcome stays visible");

// Return to the list and re-read it — the imported record surfaces
// from the server, not from local state. The record store gains it so
// the next read paints it.
recordStore["customer-9"] = { id: "customer-9", install_id: "install-crm", context_id: "ctx-a", kind: "customer", revision: 1, digest: "sha256:imp", profile: { schema: 1, display_name: "Chidi Anagonye", email: "chidi@example.com", tags: ["newcomer"], consent: { email: "granted" } }, history: [], consent_history: [] };
await click(Array.from(host.querySelectorAll("button")).find((el) => el.textContent === "Return to customers"));
await settle(() => assert(text().includes("Chidi Anagonye"), "the imported row re-reads from the list"));

// A second import with a preview that went stale refuses — the token
// binds bytes the operator actually saw planned. The fixture refuses
// any token but the one bound to the bytes above.
globalThis.fetch = (async (input: unknown, init?: RequestInit) => {
  const url = new URL(String(input), "http://localhost");
  if (url.pathname.endsWith("/records/csv-preview")) {
    return new Response(JSON.stringify({
      preview_token: "sha256:" + "c".repeat(64),
      row_count: 1,
      summary: { create: 1, update: 0, skip: 0, needs_revision: 0, error: 0 },
      rows: [{ row: 1, record_id: "customer-9", decision: "create", expected_revision: null, current_revision: null, profile: { schema: 1, display_name: "Chidi Anagonye" }, errors: [], reason: null }],
    }), { status: 200 });
  }
  if (url.pathname.endsWith("/records/csv-import")) {
    const body = JSON.parse(String(init!.body));
    csvPosts.push({ path: url.pathname, body });
    return new Response(JSON.stringify({ error: "customer CSV preview is stale; preview again" }), { status: 409 });
  }
  return priorFetch(input as RequestInfo | URL, init);
}) as typeof fetch;
await click(Array.from(host.querySelectorAll("button")).find((el) => el.textContent === "Import CSV"));
await settle(() => assert(host.querySelector("#csv-text"), "import page reopens"));
await fillArea("#csv-text", "record_id,display_name\ncustomer-9,Chidi Anagonye\n");
await click(host.querySelector('form[aria-label="CSV source"] button[type="submit"]'));
await settle(() => assert(host.querySelector("[data-preview-summary]"), "the second preview lands"));
csvPosts.length = 0;
await click(Array.from(host.querySelectorAll("button")).find((el) => (el.textContent ?? "").startsWith("Import ")));
await settle(() => assert(text().includes("stale"), "a stale preview refuses with its next step"));
assert(host.querySelector("[data-preview-summary]"), "the preview stays mounted under the refusal");
globalThis.fetch = priorFetch;

// Read-only viewers get truthful states, never forms.
await React.act(async () => { root.render(React.createElement(Harness, { viewer: { operator: true, readOnly: true } })); });
await flush(); await flush();
assert(text().includes("Read-only"), "read-only state is explicit");
assert(!host.querySelector("#crm-display-name"), "read-only renders no edit form");
assert(!Array.from(host.querySelectorAll("button")).some((el) => el.textContent === "Import CSV"), "read-only renders no import control");

await React.act(async () => { root.unmount(); });
console.log("crm customer checks passed");
}
void main();
