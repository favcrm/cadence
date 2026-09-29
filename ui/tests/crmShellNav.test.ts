export {};
/**
 * CAD-781 follow-up: F1 atomic create navigation through the real
 * AppShell, F2 narrow-viewport containment guards on the shipped CSS.
 */
declare function require(name: string): any;

function assert(value: unknown, why: string): asserts value {
  if (!value) throw new Error(why);
}

async function main() {
const { Window } = require("happy-dom");
const win = new Window({ url: "http://localhost/app-installations/install-crm?ctx=ctx-a" });
for (const name of ["window", "document", "Node", "Element", "HTMLElement", "HTMLInputElement", "HTMLSelectElement", "HTMLTextAreaElement", "SVGElement", "navigator", "MutationObserver", "ResizeObserver", "Event", "MouseEvent", "KeyboardEvent", "location", "history", "sessionStorage"])
  Object.defineProperty(globalThis, name, { value: name === "window" ? win : win[name], configurable: true, writable: true });
for (const name of ["addEventListener", "removeEventListener"])
  Object.defineProperty(globalThis, name, { value: win[name].bind(win), configurable: true });
Object.defineProperty(globalThis, "crypto", { value: require("crypto").webcrypto, configurable: true });
Object.defineProperty(globalThis, "IS_REACT_ACT_ENVIRONMENT", { value: true });
Object.defineProperty(globalThis, "EventSource", {
  configurable: true,
  value: class {
    onopen: null = null;
    onerror: null = null;
    constructor() {}
    addEventListener() {}
    removeEventListener() {}
    close() {}
  },
});
const loader = require("module"), originalRequire = loader.prototype.require;
loader.prototype.require = function (this: unknown, id: string) {
  if (id.endsWith(".css")) return {};
  if (id === "@hugeicons/core-free-icons") return new Proxy({}, { get: () => ({}) });
  if (id === "@hugeicons/react") return { HugeiconsIcon: () => null };
  return originalRequire.apply(this, arguments);
};
const React = require("react") as typeof import("react");
const { createRoot } = require("react-dom/client") as typeof import("react-dom/client");
const AppShell = (require("../src/features/app-shell/AppShell") as typeof import("../src/features/app-shell/AppShell")).default;

const install = {
  install_id: "install-crm", title: "CRM", name: "crm", version: "0.1.0",
  digest: "install-digest", catalog_generation: "gen-1", approved: true,
  storage_kind: "workspace", files: [], capabilities: null, connection_slots: [],
};
const seed = {
  id: "customer-s1", install_id: "install-crm", context_id: "ctx-a", kind: "customer",
  revision: 1, digest: "sha256:seed",
  profile: { schema: 1, display_name: "Seed Alpha", email: "seed-alpha@example.com", tags: ["vip"], consent: { email: "granted", sms: "unknown" } },
  history: [{ revision: 1, digest: "sha256:seed", actor: "operator", at: 1759113600 }],
  consent_history: [{ revision: 1, channel: "email", state: "granted", actor: "operator", at: 1759113600 }],
};
const created: Record<string, any> = {};
const json = (value: unknown, status = 200) => new Response(JSON.stringify(value), { status, headers: { "Content-Type": "application/json" } });
globalThis.fetch = (async (input: unknown, init?: RequestInit) => {
  const path = String(input);
  const url = new URL(path, "http://localhost");
  if (init?.method === "POST") {
    if (url.pathname.endsWith("/records")) {
      const body = JSON.parse(String(init.body));
      const record = {
        id: body.record_id, install_id: "install-crm", context_id: "ctx-a", kind: "customer",
        revision: 1, digest: "sha256:new", profile: body.profile,
        history: [{ revision: 1, digest: "sha256:new", actor: "operator", at: 1759286400 }],
        consent_history: [{ revision: 1, channel: "email", state: body.profile.consent.email, actor: "operator", at: 1759286400 }],
      };
      created[body.record_id] = record;
      return json({ record });
    }
    return json({ ok: true });
  }
  if (path === "/api/app-installations/install-crm") return json(install);
  if (path === "/api/app-installations/install-crm/contexts") return json({ contexts: [
    { id: "ctx-a", install_id: "install-crm", revision: 1, state: "active", digest: "ca", config: { schema: 1, label: "Acme", input_defaults: {} } },
  ] });
  if (path.startsWith("/api/threads/master")) return json({ entries: [], more_before: false });
  if (url.pathname.endsWith("/records")) return json({ records: [seed], truncated: false, next_cursor: null });
  const id = url.pathname.split("/").pop()!;
  if (created[id]) return json({ record: created[id] });
  if (id === "customer-s1") return json({ record: seed });
  throw new Error(`Unexpected read ${path}`);
}) as typeof fetch;

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
async function fill(selector: string, value: string) {
  const element = host.querySelector(selector) as HTMLInputElement;
  assert(element, `field exists: ${selector}`);
  await React.act(async () => {
    Object.getOwnPropertyDescriptor(HTMLInputElement.prototype, "value")!.set!.call(element, value);
    element.dispatchEvent(new Event("input", { bubbles: true }));
  });
  await flush();
}

win.sessionStorage.clear();
history.pushState(null, "", "/app-installations/install-crm?ctx=ctx-a");
await React.act(async () => {
  root.render(React.createElement(AppShell, { installId: "install-crm", viewer: { operator: true, readOnly: false } }));
});
await settle(() => assert(text().includes("Seed Alpha"), "shell list paints server rows"));
assert(text().includes("Customers"), "CRM section renders inside the real shell");

// Pure section-link grammar: context (and any other host param)
// survives, the record view and New form never follow a move, and
// Customers is the bare default route.
const { crmSectionHref } = require("../src/features/app-shell/CrmOutlet") as typeof import("../src/features/app-shell/CrmOutlet");
assert(
  crmSectionHref("/app-installations/install-crm?ctx=ctx-a", "segments") ===
    "/app-installations/install-crm?ctx=ctx-a&crm=segments",
  "segment href keeps the selected context",
);
assert(
  crmSectionHref("/app-installations/install-crm?ctx=ctx-a&crm=segments&record=r1&appview=new", "customers") ===
    "/app-installations/install-crm?ctx=ctx-a",
  "customers href drops the section, record and form",
);
assert(
  crmSectionHref("/app-installations/install-crm?ctx=ctx-a&crm=segments", "campaigns") ===
    "/app-installations/install-crm?ctx=ctx-a&crm=campaigns",
  "cross-section href swaps only the section",
);
assert(
  crmSectionHref("/app-installations/install-crm", "customers") === "/app-installations/install-crm",
  "bare default route stays bare",
);

// Host submenu: Apps → CRM → sections lives in the shared outlet
// as real links (openable, copyable, keyboard-focusable) with the
// current section marked — not buttons inside the CRM pane.
const submenu = () => host.querySelector('nav[aria-label="CRM sections"]');
assert(submenu(), "host-owned CRM submenu renders in the shell");
const submenuLinks = Array.from(submenu()!.querySelectorAll("a"));
assert(
  submenuLinks.length === 3 && submenuLinks.every((el) => el.tagName === "A"),
  "submenu offers three real links",
);
assert(
  submenuLinks[1].getAttribute("href") === "/app-installations/install-crm?ctx=ctx-a&crm=segments",
  "segment link keeps the selected context",
);
assert(
  submenu()!.querySelector('a[aria-current="page"]')?.textContent?.trim() === "Customers",
  "submenu marks the current section",
);
submenuLinks[1].focus();
assert(document.activeElement === submenuLinks[1], "submenu links take keyboard focus");
// One chat node across section moves; a drawer never follows.
const openSeed = Array.from(host.querySelectorAll("button.lnk")).find(
  (el) => el.textContent === "Open" && el.closest("tr")?.textContent?.includes("Seed Alpha"),
);
await click(openSeed);
await settle(() => assert(host.querySelector('[data-drawer="customer"]'), "drawer opens before the move"));
const chatBefore = host.querySelector("[data-chat-pane]");
assert(chatBefore, "chat pane mounts with the shell");
await click(submenuLinks[1]);
await settle(() => assert(location.search.includes("crm=segments"), "submenu link routes"));
assert(!host.querySelector("[data-drawer]"), "no drawer follows a section move");
assert(host.querySelector("[data-chat-pane]") === chatBefore, "section move keeps the single chat node mounted");
assert(
  submenu()!.querySelector('a[aria-current="page"]')?.textContent?.trim() === "Segments",
  "submenu follows the route",
);
await click(Array.from(submenu()!.querySelectorAll("a")).find((el) => (el.textContent ?? "").trim() === "Customers")!);
await settle(() => assert(text().includes("Seed Alpha"), "customers link restores the list"));
const wrap = host.querySelector(".crm-table-wrap");
assert(wrap?.getAttribute("tabindex") === "0", "table wrap is keyboard-focusable for internal scroll");
assert(wrap?.getAttribute("role") === "region", "table wrap is a labelled scroll region");
assert((wrap?.getAttribute("aria-label") ?? "").length > 0, "table wrap names its scroll affordance");

// F1: New → Create lands on list + details in ONE history push with no
// stale `appview=new`. Two sequential writes would push twice and keep
// `appview=new` (the reported defect).
await click(Array.from(host.querySelectorAll("button")).find((el) => el.textContent === "New customer"));
await settle(() => assert(host.querySelector("#crm-display-name"), "new page opens"));
assert(location.search.includes("appview=new"), "new route is in the URL before create");
await fill("#crm-display-name", "Browser QA Delta");
await fill("#crm-email", "delta.qa@example.test");
const pushesBefore = history.length;
await click(host.querySelector('section[aria-label="New customer"] button[type="submit"]'));
await settle(() => assert(host.querySelector('[data-drawer="customer"]'), "created record opens its drawer"));
assert(location.search.includes("record="), "record scope lands in the URL");
assert(!location.search.includes("appview="), "no stale appview=new after create");
assert(text().includes("Seed Alpha"), "the underlying page is the Customers list again");
assert(text().includes("Browser QA Delta"), "the drawer shows the created record");
assert(
  history.length === pushesBefore + 1,
  `create navigates atomically in one push (pushed ${history.length - pushesBefore})`,
);

// F1 reload coherence: the landed URL restores list + drawer directly.
const landed = location.href;
await React.act(async () => { root.unmount(); });
await React.act(async () => {
  history.pushState(null, "", landed.replace("http://localhost", ""));
  const root2 = createRoot(host);
  root2.render(React.createElement(AppShell, { installId: "install-crm", viewer: { operator: true, readOnly: false } }));
  (globalThis as any).__crmRoot2 = root2;
});
await settle(() => assert(
  host.querySelector('[data-drawer="customer"]') && text().includes("Seed Alpha"),
  "reload of the landed URL restores list + drawer",
));
await React.act(async () => { (globalThis as any).__crmRoot2.unmount(); });

// F2: the shipped CSS keeps every scroll container and grid item inside
// the viewport. happy-dom has no layout engine, so this guards the
// exact properties the 390px pass measured; the operator re-verifies
// `documentElement.scrollWidth <= innerWidth` in a real browser.
const fs = require("fs");
const path = require("path");
const proc = (globalThis as any).process;
const cssPath = path.join(proc.cwd(), "src/features/app-shell/app-shell.css");
assert(fs.existsSync(cssPath), `shipped CSS present at ${cssPath}`);
const css: string = fs.readFileSync(cssPath, "utf8");
const block = (selector: string): string => {
  const match = css.match(new RegExp(`${selector}\\s*\\{([^}]*)\\}`));
  assert(match, `CSS block exists: ${selector}`);
  return match![1];
};
assert(/min-width:\s*0/.test(block("\\.app-outlet")), ".app-outlet can shrink inside the shell grid");
assert(/min-width:\s*0/.test(block("\\.crm-list")), ".crm-list can shrink");
assert(/overflow-x:\s*auto/.test(block("\\.crm-table-wrap")), "table scrolls inside its wrap");
assert(/contain:\s*layout paint/.test(block("\\.crm-table-wrap")), "wrap contains the wide table's paint");
assert(/min-width:\s*680px/.test(block("\\.crm-table")), "table keeps readable columns via a width floor");
assert(/max-width:\s*100%/.test(block("\\.crm-table-wrap")), "wrap never exceeds its column");
assert(/overflow-wrap:\s*anywhere/.test(block("\\.crm-table th,\\s*\\.crm-table td")), "unbreakable tokens wrap in table cells");
assert(/flex-wrap:\s*wrap/.test(block("\\.app-outlet-tabs")), "section tabs wrap at narrow widths");
assert(/flex-wrap:\s*wrap/.test(block("\\.crm-pager")), "pager wraps at narrow widths");
// The host submenu reuses the shared section-nav kit: it wraps
// instead of overflowing at narrow widths and never forces a
// minimum width on the shell grid.
const stylesPath = path.join(proc.cwd(), "src/styles.css");
assert(fs.existsSync(stylesPath), `shared styles present at ${stylesPath}`);
const stylesCss: string = fs.readFileSync(stylesPath, "utf8");
const stylesBlock = (selector: string): string => {
  const match = stylesCss.match(new RegExp(`${selector}\\s*\\{([^}]*)\\}`));
  assert(match, `CSS block exists: ${selector}`);
  return match![1];
};
assert(/flex-wrap:\s*wrap/.test(stylesBlock(".section-nav")), "host submenu wraps at narrow widths");
assert(/min-width:\s*0/.test(stylesBlock(".section-nav")), "host submenu can shrink inside the shell grid");
// CAD-784: the campaign preview source scrolls inside its own box
// instead of pushing the document past narrow viewports, and the
// scriptless visual frame never exceeds its column.
assert(/max-width:\s*100%/.test(block("\\.crm-preview")), "preview source never exceeds its column");
assert(/overflow:\s*auto/.test(block("\\.crm-preview")), "preview source scrolls internally");
assert(/width:\s*100%/.test(block("\\.crm-preview-frame")), "visual frame fills but never exceeds its column");

console.log("crm shell nav checks passed");
}
void main();
