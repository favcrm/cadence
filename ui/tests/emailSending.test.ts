export {};
/**
 * CAD-1059 / CAD-1063 slice 2: Settings -> Email sending, mounted.
 * One page chooses the CRM sender (SMTP or hosted AgenticOS), binds or
 * rebinds it with the existing crm_smtp verbs, and sets the unsubscribe
 * origin once. Link/auth revisions stay behind Details.
 */
declare function require(name: string): any;

function assert(value: unknown, why: string): asserts value {
  if (!value) throw new Error(why);
}
function equal(actual: unknown, expected: unknown, why: string): void {
  const a = JSON.stringify(actual), e = JSON.stringify(expected);
  if (a !== e) throw new Error(`${why}: expected ${e}, got ${a}`);
}

interface World {
  connections: any[];
  binding: any | null;
  contexts: any[];
  origin: { unsubscribe_origin: string | null; stored: boolean };
  posts: { path: string; body: any }[];
}

const smtpRow = (id: string, over: any = {}) => ({
  id, provider: "smtp", account: id, kind: "enrolled", revision: 1, scopes: [], smtp_sender: true, smtp_error: null,
  smtp: { host: "smtp.example.com", port: 587, tls_mode: "starttls", username: "mailer", sender: `${id}@example.com`, sender_name: "News" },
  status: { adapter_registered: true, descriptor_available: true, custody_available: true, manifest_status: "matched" },
  ...over,
});
const hostedRow = () => smtpRow("builtin-hosted", {
  provider: "agenticos", account: "hosted", kind: "builtin",
  smtp: { host: "api.internal", port: 0, tls_mode: "platform", username: "", sender: "acme@cadencecloud.app", sender_name: "" },
});
const bindingFor = (row: any, over: any = {}) => ({
  install_id: "install-crm", context_id: "ctx-a", connection_id: row.id, auth_revision: 7, link_revision: 4, state: "live",
  digest: "d".repeat(64),
  sender: { name: row.smtp.sender_name, address: row.smtp.sender },
  transport: { host: row.smtp.host, port: row.smtp.port, tls_mode: row.smtp.tls_mode, username: row.smtp.username },
  transport_kind: row.smtp.tls_mode === "platform" ? "agenticos" : "smtp",
  ...over,
});

async function mount(world: World, viewer = { operator: true, readOnly: false }) {
  const { Window } = require("happy-dom");
  const win = new Window({ url: "http://localhost/settings/email" });
  for (const name of ["window", "document", "Node", "Element", "HTMLElement", "HTMLInputElement", "HTMLTextAreaElement", "HTMLSelectElement", "SVGElement", "navigator", "MutationObserver", "ResizeObserver", "Event", "MouseEvent", "KeyboardEvent", "location", "history", "sessionStorage"])
    Object.defineProperty(globalThis, name, { value: name === "window" ? win : win[name], configurable: true, writable: true });
  for (const name of ["addEventListener", "removeEventListener"])
    Object.defineProperty(globalThis, name, { value: win[name].bind(win), configurable: true });
  Object.defineProperty(globalThis, "IS_REACT_ACT_ENVIRONMENT", { value: true, configurable: true });
  Object.defineProperty(globalThis, "requestAnimationFrame", { value: (fn: () => void) => setTimeout(fn, 0), configurable: true });
  const loader = require("module"), originalRequire = loader.prototype.require;
  loader.prototype.require = function (this: unknown, id: string) {
    if (id.endsWith(".css")) return {};
    if (id === "@hugeicons/core-free-icons") return new Proxy({}, { get: () => ({}) });
    if (id === "@hugeicons/react") return { HugeiconsIcon: () => null };
    return originalRequire.apply(this, arguments);
  };
  const React = require("react");
  const { createRoot } = require("react-dom/client");
  const EmailSending = require("../src/features/settings/EmailSending").default;
  const json = (value: unknown, status = 200) => new Response(JSON.stringify(value), { status, headers: { "Content-Type": "application/json" } });
  globalThis.fetch = (async (input: unknown, init?: RequestInit) => {
    const url = new URL(String(input), "http://localhost");
    const method = init?.method ?? "GET";
    const body = init?.body ? JSON.parse(String(init.body)) : undefined;
    if (method === "POST") world.posts.push({ path: url.pathname, body });
    switch (`${method} ${url.pathname}`) {
      case "GET /api/app-installations":
        return json([{ install_id: "install-crm", name: "crm", title: "CRM" }, { install_id: "install-social", name: "social-content", title: "Social" }]);
      case "GET /api/app-installations/install-crm/contexts": return json({ contexts: world.contexts });
      case "GET /api/connections": return json({ connections: world.connections });
      case "POST /api/crm-smtp/show":
        return world.binding ? json({ binding: world.binding }) : json({ error: "no SMTP sender is bound to this context" }, 409);
      case "GET /api/crm-send/origin": return json(world.origin);
      case "POST /api/crm-send/origin":
        world.origin = { unsubscribe_origin: body.unsubscribe_origin, stored: body.unsubscribe_origin !== null };
        return json(world.origin);
      case "POST /api/crm-smtp/bind": {
        const row = world.connections.find((r) => r.id === body.connection_id);
        world.binding = bindingFor(row);
        return json({ binding: world.binding });
      }
      case "POST /api/crm-smtp/rebind": {
        if (body.expected_revision !== world.binding.link_revision) return json({ error: "stale link revision" }, 409);
        const row = world.connections.find((r) => r.id === body.connection_id);
        world.binding = bindingFor(row, { link_revision: world.binding.link_revision + 1 });
        return json({ binding: world.binding });
      }
      case "POST /api/crm-smtp/revoke": world.binding = null; return json({ ok: true });
    }
    throw new Error(`Unexpected ${method} ${url.pathname}`);
  }) as typeof fetch;
  const host = document.createElement("div");
  document.body.append(host);
  const root = createRoot(host);
  const sleep = (ms: number) => new Promise((r) => setTimeout(r, ms));
  const flush = () => React.act(async () => { await sleep(0); });
  const settle = async (check: () => void, budget = 6000) => {
    const end = Date.now() + budget;
    for (;;) {
      await flush();
      try { check(); return; } catch (e) { if (Date.now() > end) throw e; await sleep(60); }
    }
  };
  const click = async (el: Element | null | undefined) => {
    assert(el, "click target exists");
    await React.act(async () => { el.dispatchEvent(new MouseEvent("click", { bubbles: true })); });
    await flush();
  };
  const button = (label: string) =>
    Array.from(host.querySelectorAll("button")).find((b) => (b.textContent ?? "").trim() === label) as HTMLButtonElement | undefined;
  await React.act(async () => { root.render(React.createElement(EmailSending, { viewer })); });
  const mainText = () => {
    const clone = host.cloneNode(true) as HTMLElement;
    clone.querySelectorAll("details").forEach((d) => d.remove());
    return clone.textContent ?? "";
  };
  return { host, settle, click, button, sleep, flush, React, mainText, unmount: () => React.act(async () => root.unmount()) };
}

const fresh = (over: Partial<World> = {}): World => ({
  connections: [smtpRow("conn-a")], binding: null,
  contexts: [{ id: "ctx-a", state: "active", config: { label: "Acme" } }],
  origin: { unsubscribe_origin: null, stored: false }, posts: [], ...over,
});

async function main() {
  // SMTP sender: host/port/username/sender shown; bind sends only ids.
  {
    const world = fresh();
    const m = await mount(world);
    await m.settle(() => assert(m.button("Use this sender"), "bind button"));
    await m.click(m.host.querySelector('[role="combobox"]'));
    const opts = Array.from(document.querySelectorAll('[role="option"]')).map((o) => o.textContent ?? "");
    assert(opts.some((t) => t.includes("conn-a@example.com") && t.includes("smtp.example.com:587") && t.includes("login mailer")), "SMTP sender host/port/username/sender in the option");
    assert(m.host.querySelectorAll("[data-state=no-crm]").length === 0, "no empty state");
    assert(m.host.querySelector("#email-sending-target") === null, "single context has no selector");
    await m.click(m.button("Use this sender"));
    await m.settle(() => assert(m.host.querySelector("[data-state=live]")?.textContent?.includes("Sending from conn-a@example.com"), "live status"));
    const bind = world.posts.find((p) => p.path === "/api/crm-smtp/bind")!;
    equal(Object.keys(bind.body).sort(), ["connection_id", "context_id", "install_id", "request_id"], "bind body carries ids only, no revisions");
    await m.unmount();
  }
  // Hosted sender label; its row has no password field; main path hides revisions.
  {
    const row = hostedRow();
    const world = fresh({ connections: [row], binding: bindingFor(row) });
    const m = await mount(world);
    await m.settle(() => assert(m.host.querySelector("[data-state=live]"), "live status"));
    equal(m.host.querySelector("[data-state=live]")!.textContent!.replace(/\s+/g, " ").trim(), "Sending via AgenticOS — acme@cadencecloud.app ✓", "hosted label");
    const main = m.mainText();
    assert(!/link r4|auth r7|d{18}/.test(main) && !main.includes("ddddddddd"), "revisions and digests are not in the main path");
    const details = m.host.querySelector("details[data-details=sender]")!;
    assert(details.textContent!.includes("link r4") && details.textContent!.includes("auth r7"), "Details holds the revisions");
    assert(m.host.querySelector("input[type=password]") === null, "no password field");
    await m.unmount();
  }
  // Unusable SMTP sender: typed reason and a link to Connections.
  {
    const bad = smtpRow("conn-bad", { smtp: null, smtp_error: "withheld_leak" });
    const world = fresh({ connections: [bad] });
    const m = await mount(world);
    await m.settle(() => assert(m.host.querySelector("[data-state=sender-unusable]"), "unusable row"));
    const li = m.host.querySelector("[data-state=sender-unusable]")!;
    assert(li.textContent!.includes("Settings are hidden: your password shares characters"), "typed smtp_error reason");
    assert(li.querySelector('a[href="/settings/connections"]'), "link to Connections");
    assert(m.button("Use this sender") === undefined, "an unusable sender cannot be bound");
    await m.unmount();
  }
  // Rebind: the hidden link revision rides the request; a revoke clears.
  {
    const a = smtpRow("conn-a"), b = smtpRow("conn-b");
    const world = fresh({ connections: [a, b], binding: bindingFor(a) });
    const m = await mount(world);
    await m.settle(() => assert(m.button("Current sender"), "current sender shown, disabled"));
    assert((m.button("Current sender") as HTMLButtonElement).disabled, "rebinding to the same live sender is a no-op");
    await m.click(m.host.querySelector('[role="combobox"]'));
    await m.click(Array.from(document.querySelectorAll('[role="option"]')).find((o) => (o.textContent ?? "").includes("conn-b@example.com")));
    await m.click(m.button("Switch to this sender"));
    await m.settle(() => assert(m.host.querySelector("[data-state=live]")?.textContent?.includes("conn-b@example.com"), "rebound"));
    const rebind = world.posts.find((p) => p.path === "/api/crm-smtp/rebind")!;
    equal(rebind.body, { install_id: "install-crm", context_id: "ctx-a", connection_id: "conn-b", expected_revision: 4 }, "rebind carries the observed link revision");
    await m.click(m.button("Stop using"));
    await m.click(Array.from(document.querySelectorAll("button")).filter((x) => (x.textContent ?? "").trim() === "Stop using").pop());
    await m.settle(() => assert(m.host.querySelector("[data-state=unbound]"), "revoked"));
    equal(world.posts.find((p) => p.path === "/api/crm-smtp/revoke")!.body.expected_revision, 5, "revoke pins the new link revision");
    await m.unmount();
  }
  // A stale binding asks for a re-check instead of claiming sending.
  {
    const a = smtpRow("conn-a");
    const world = fresh({ connections: [a], binding: bindingFor(a, { state: "stale" }) });
    const m = await mount(world);
    await m.settle(() => assert(m.host.querySelector("[data-state=stale]"), "stale notice"));
    assert(m.host.querySelector("[data-state=live]") === null, "no sending claim on a stale binding");
    await m.unmount();
  }
  // Unsubscribe origin: set once, shown as set, then cleared.
  {
    const world = fresh();
    const m = await mount(world);
    await m.settle(() => assert(m.host.querySelector("#email-origin"), "origin field"));
    const input = m.host.querySelector("#email-origin") as HTMLInputElement;
    await m.React.act(async () => {
      const setter = Object.getOwnPropertyDescriptor(Object.getPrototypeOf(input), "value")!.set!;
      setter.call(input, "https://cadence.example.com");
      input.dispatchEvent(new Event("input", { bubbles: true }));
    });
    await m.click(m.button("Save"));
    await m.settle(() => assert(m.host.textContent!.includes("Unsubscribe links now use https://cadence.example.com"), "origin saved"));
    equal(world.posts.find((p) => p.path === "/api/crm-send/origin")!.body, { unsubscribe_origin: "https://cadence.example.com" }, "origin body");
    assert(m.host.textContent!.includes("(set here)"), "marked as set here");
    await m.unmount();
  }
  // Several CRM contexts: a selector appears; no CRM: honest empty state.
  {
    const world = fresh({ contexts: [{ id: "ctx-a", state: "active", config: { label: "Acme" } }, { id: "ctx-b", state: "active", config: { label: "Beta" } }, { id: "ctx-x", state: "archived", config: { label: "Old" } }] });
    const m = await mount(world);
    await m.settle(() => assert(m.host.querySelector('[aria-label="CRM context"]'), "context selector"));
    await m.click(m.host.querySelector('[aria-label="CRM context"][role="combobox"]'));
    const labels = Array.from(document.querySelectorAll('[role="option"]')).map((o) => o.textContent);
    equal(labels, ["CRM · Acme", "CRM · Beta"], "only active contexts are offered");
    await m.unmount();
  }
  // Read-only viewers see the state but get no write controls.
  {
    const a = smtpRow("conn-a");
    const world = fresh({ connections: [a], binding: bindingFor(a) });
    const m = await mount(world, { operator: true, readOnly: true });
    await m.settle(() => assert(m.host.querySelector("[data-state=live]"), "status visible"));
    assert(!m.button("Stop using") && !m.button("Save") && !m.host.querySelector("#email-origin"), "no write controls");
    await m.unmount();
  }
  console.log("email sending checks passed");
}
void main();
