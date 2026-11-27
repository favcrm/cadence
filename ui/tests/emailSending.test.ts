export {};
/**
 * Settings → Connections → Email, mounted.
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
  hostedSmtp?: boolean;
  binding: any | null;
  /** Per-context bindings; a missing key falls back to `binding`. */
  bindings?: Record<string, any | null>;
  installations?: any[];
  contexts: any[];
  origin: { unsubscribe_origin: string | null; stored: boolean };
  posts: { path: string; body: any }[];
  failConnectionsOnce?: boolean;
  denyConnections?: boolean;
  failBindOnce?: boolean;
  connectionsPartial?: boolean;
  failContextsOnce?: boolean;
  reads?: string[];
  connectionQueries?: string[];
  failShowOnce?: boolean;
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

async function mount(world: World, viewer = { operator: true, readOnly: false }, fixedUnsubscribeAddress = false, accountId?: string) {
  const { Window } = require("happy-dom");
  const win = new Window({ url: "http://localhost/settings/connections/email" });
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
  const emailModule = require("../src/features/settings/EmailSending");
  const EmailSendingView = emailModule.default;
  const session = require("../src/lib/privateSession");
  session.resetPrivateSession();
  session.allowPrivateReads();
  const { NavigationProvider } = require("../screen-ui/src/navigation") as typeof import("../screen-ui/src/navigation");
  const { hostNavigation } = require("../src/ui/hostNavigation") as typeof import("../src/ui/hostNavigation");
  const EmailSending = (props: React.ComponentProps<typeof EmailSendingView>) =>
    React.createElement(NavigationProvider, { adapter: hostNavigation, children: React.createElement(EmailSendingView, props) });
  const json = (value: unknown, status = 200) => new Response(JSON.stringify(value), { status, headers: { "Content-Type": "application/json" } });
  globalThis.fetch = (async (input: unknown, init?: RequestInit) => {
    const url = new URL(String(input), "http://localhost");
    const method = init?.method ?? "GET";
    const body = init?.body ? JSON.parse(String(init.body)) : undefined;
    if (method === "GET") world.reads?.push(url.pathname);
    if (method === "GET" && url.pathname === "/api/connections") world.connectionQueries?.push(url.search);
    if (method === "POST") world.posts.push({ path: url.pathname, body });
    switch (`${method} ${url.pathname}`) {
      case "GET /api/protected-entry": return json({ configured: false, origin: null });
      case "GET /api/app-installations":
        return json(world.installations ?? [{ install_id: "install-crm", name: "crm", title: "CRM", smtp_custody: true }, { install_id: "install-social", name: "social-content", title: "Social" }]);
      case "GET /api/app-installations/install-crm/contexts":
        if (world.failContextsOnce) {
          world.failContextsOnce = false;
          return json({ error: "contexts are unavailable" }, 503);
        }
        return json({ contexts: world.contexts });
      case "GET /api/connections":
        if (world.denyConnections) return json({ error: { code: "operator_required", message: "Operator required" } }, 403);
        if (!fixedUnsubscribeAddress && url.search) return json({ error: "connection query parameters are unsupported" }, 400);
        if (world.failConnectionsOnce) {
          world.failConnectionsOnce = false;
          return json({ error: "connections are unavailable" }, 503);
        }
        return json({
          connections: world.connections, hosted_smtp: world.hostedSmtp,
          ...(world.connectionsPartial ? { connections_unavailable: { code: "unavailable" } } : {}),
        });
      case "POST /api/crm-smtp/show": {
        if (world.failShowOnce) {
          world.failShowOnce = false;
          return json({ error: "the sender is unavailable" }, 503);
        }
        const found = world.bindings && Object.hasOwn(world.bindings, body.context_id)
          ? world.bindings[body.context_id]
          : world.binding;
        return found ? json({ binding: found }) : json({ error: "no SMTP sender is bound to this context" }, 409);
      }
      case "GET /api/crm-send/origin": return json(world.origin);
      case "POST /api/crm-send/origin":
world.origin = { unsubscribe_origin: body.unsubscribe_origin, stored: body.unsubscribe_origin !== null };
        // As the daemon does: the receipt names the value, and only a read says `stored`.
        return json({ unsubscribe_origin: body.unsubscribe_origin });
      case "POST /api/crm-smtp/bind": {
        if (world.failBindOnce) {
          world.failBindOnce = false;
          return json({ error: "The sender could not be saved." }, 503);
        }
        const row = world.connections.find((r) => r.id === body.connection_id);
        const next = bindingFor(row);
        next.context_id = body.context_id;
        world.binding = next;
        world.bindings = { ...world.bindings, [body.context_id]: next };
        return json({ binding: next });
      }
      case "POST /api/crm-smtp/rebind": {
        if (body.expected_revision !== world.binding.link_revision) return json({ error: "stale link revision" }, 409);
        const row = world.connections.find((r) => r.id === body.connection_id);
        const next = bindingFor(row, { link_revision: world.binding.link_revision + 1 });
        next.context_id = body.context_id;
        world.binding = next;
        world.bindings = { ...world.bindings, [body.context_id]: next };
        return json({ binding: next });
      }
      case "POST /api/crm-smtp/revoke": {
        world.binding = null;
        if (world.bindings) delete world.bindings[body.context_id];
        return json({ ok: true });
      }
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
    Array.from(document.querySelectorAll("button")).find((b) => (b.textContent ?? "").trim() === label) as HTMLButtonElement | undefined;
  const render = async (focusSenders = false) => {
    const view = accountId
      ? React.createElement(emailModule.EmailAccount, { id: accountId, viewer, hostedBoard: fixedUnsubscribeAddress })
      : React.createElement(EmailSending, { viewer, fixedUnsubscribeAddress, focusSenders });
    await React.act(async () => { root.render(accountId ? React.createElement(NavigationProvider, { adapter: hostNavigation, children: view }) : view); });
  };
  await render();
  // A row is a name and one line; everything else is in the drawer it opens.
  const open = async (name: string) => {
    await settle(() => assert(rowFor(name), `row ${name}`));
    await click(rowFor(name));
  };
  const rowFor = (name: string) =>
    Array.from(host.querySelectorAll("button.settings-row")).find((b) => b.querySelector("b")?.textContent === name);
  const closeDrawer = async () => {
    await click(Array.from(document.querySelectorAll("button")).find((b) => /close/i.test(b.getAttribute("aria-label") ?? b.textContent ?? "")));
    await sleep(320);
    await flush();
  };
  const mainText = () => {
    const clone = document.body.cloneNode(true) as HTMLElement;
    clone.querySelectorAll("details").forEach((d) => d.remove());
    return clone.textContent ?? "";
  };
  return {
    host: document.body,
    settle,
    click,
    button,
    open,
    rowFor,
    closeDrawer,
    sleep,
    flush,
    React,
    mainText,
    render,
    showAccount: async (id: string) => { accountId = id; await render(); },
    unmount: async () => {
      await React.act(async () => root.unmount());
      host.remove();
    },
  };
}

const fresh = (over: Partial<World> = {}): World => ({
  connections: [smtpRow("conn-a")], binding: null,
  contexts: [{ id: "ctx-a", state: "active", config: { label: "Acme" } }],
  origin: { unsubscribe_origin: null, stored: false }, bindings: {}, posts: [], ...over,
});

async function main() {
  // SMTP sender: host/port/username/sender shown; bind sends only ids.
  {
    const world = fresh();
    const m = await mount(world);
    await m.open("CRM · Acme");
    await m.settle(() => assert(m.button("Use this sender"), "bind button"));
    await m.click(m.button("Use this sender"));
    await m.settle(() => assert(m.host.querySelector("[data-state=live]")?.textContent?.includes("Sending from conn-a@example.com"), "live status"));
    const bind = world.posts.find((p) => p.path === "/api/crm-smtp/bind")!;
    equal(Object.keys(bind.body).sort(), ["connection_id", "context_id", "install_id", "request_id"], "bind body carries ids only, no revisions");
    await m.unmount();
  }
  // Every eligible context keeps its own binding. Account credentials
  // are only entered after opening Add email.
  {
    const a = smtpRow("conn-a"), b = smtpRow("conn-b");
    const world = fresh({
      connections: [a, b],
      contexts: [
        { id: "ctx-a", state: "active", config: { label: "Acme" } },
        { id: "ctx-b", state: "active", config: { label: "Billing" } },
      ],
      bindings: { "ctx-a": bindingFor(a) },
    });
    const m = await mount(world);
    await m.settle(() => {
      assert(m.rowFor("CRM · Acme")?.textContent?.includes("conn-a@example.com"), "the bound context names its sender");
      assert(m.rowFor("CRM · Billing")?.textContent?.includes("No sender chosen"), "the other context is unbound");
    });
    assert(m.host.querySelector('a[href="/settings/connections/email/account/conn-a"]'), "a listed SMTP account can be managed");
    assert(!m.host.querySelector("input[type=password]"), "no password field on the policy page");
    await m.open("CRM · Billing");
    await m.settle(() => assert(m.button("Use this sender"), "bind button"));
    await m.click(m.button("Use this sender"));
    await m.settle(() => assert(m.host.querySelector("[data-state=live]"), "the second context binds"));
    const bind = world.posts.find((p) => p.path === "/api/crm-smtp/bind")!;
    equal(bind.body.context_id, "ctx-b", "the bind carries the second context");
    await m.unmount();
  }
  // CAD-1797 review: a disabled SMTP-capable install must not blank the
  // active app's sender list or the workspace unsubscribe policy.
  {
    const world = fresh({
      installations: [
        { install_id: "install-crm", name: "crm", title: "CRM", smtp_custody: true, state: "active" },
        { install_id: "install-disabled", name: "legacy", title: "Legacy", smtp_custody: true, state: "disabled" },
      ],
      binding: bindingFor(smtpRow("conn-a")),
    });
    const m = await mount(world);
    await m.settle(() => {
      assert(m.rowFor("CRM · Acme")?.textContent?.includes("conn-a@example.com"), "active sender renders with a disabled SMTP install");
      assert(m.rowFor("Unsubscribe links"), "workspace policy renders with a disabled SMTP install");
      assert(!(m.host.textContent ?? "").includes("Unknown installation"), "disabled install is not read");
    });
    await m.unmount();
  }
  // Hosted sender label; its row has no password field; main path hides revisions.
  {
    const row = hostedRow();
    const world = fresh({ connections: [row], binding: bindingFor(row) });
    const m = await mount(world);
    await m.open("CRM · Acme");
    await m.settle(() => assert(m.host.querySelector("[data-state=live]"), "live status"));
    assert(m.host.querySelector("[data-state=live]")!.textContent!.includes("acme@cadencecloud.app"), "hosted sender address shown");
    const main = m.mainText();
    assert(!/link r4|auth r7|d{18}/.test(main) && !main.includes("ddddddddd"), "revisions and digests are not in the main path");
    const details = m.host.querySelector("details[data-details=sender]")!;
    assert(details.textContent!.includes("link r4") && details.textContent!.includes("auth r7"), "Details holds the revisions");
    assert(m.host.querySelector("input[type=password]") === null, "no password field");
    await m.unmount();
  }
  // Unusable SMTP sender: never bindable, typed reason, link to Email accounts.
  {
    const bad = smtpRow("conn-bad", { smtp: null, smtp_error: "withheld_leak" });
    const m = await mount(fresh({ connections: [bad] }));
    await m.open("CRM · Acme");
    await m.settle(() => assert(m.host.querySelector("[data-state=sender-unusable]"), "unusable row"));
    const li = m.host.querySelector("[data-state=sender-unusable]")!;
    assert(li.textContent!.includes("password shares characters"), "typed smtp_error reason");
    assert(li.querySelector('a[href="/settings/connections/email"]'), "link to Email accounts");
    assert(m.button("Use this sender") === undefined, "an unusable sender cannot be bound");
    await m.unmount();
  }
  // Rebind: the hidden link revision rides the request; a revoke clears.
  {
    const a = smtpRow("conn-a"), b = smtpRow("conn-b");
    const world = fresh({ connections: [a, b], binding: bindingFor(a) });
    const m = await mount(world);
    await m.open("CRM · Acme");
    await m.settle(() => assert(m.host.querySelector("[data-state=live]"), "bound"));
    assert(m.button("Current sender") === undefined, "the current sender is a statement, not a button");
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
    await m.settle(() => assert(m.rowFor("CRM · Acme")?.textContent?.includes("Needs attention"), "the row flags it"));
    await m.open("CRM · Acme");
    await m.settle(() => assert(m.host.querySelector("[data-state=stale]"), "stale notice"));
    assert(m.host.querySelector("[data-state=live]") === null, "no sending claim on a stale binding");
    await m.unmount();
  }
  // Unsubscribe origin: set once, shown as set, then cleared.
  {
    const world = fresh();
    const m = await mount(world);
    await m.open("Unsubscribe links");
    await m.settle(() => assert(m.host.querySelector("#email-origin"), "origin field"));
    assert(m.button("Save") === undefined, "Save appears only after a change");
    const input = m.host.querySelector("#email-origin") as HTMLInputElement;
    await m.React.act(async () => {
      const setter = Object.getOwnPropertyDescriptor(Object.getPrototypeOf(input), "value")!.set!;
      setter.call(input, "https://cadence.example.com");
      input.dispatchEvent(new Event("input", { bubbles: true }));
    });
    await m.click(m.button("Save"));
    await m.settle(() => assert(m.host.textContent!.includes("Unsubscribe links now use https://cadence.example.com"), "origin saved"));
    await m.settle(() => assert(m.host.textContent!.includes("(set here)"), "a read confirms it is stored"));
    assert(m.button("Clear"), "a stored address can be cleared");
    await m.click(m.button("Clear"));
    await m.settle(() => assert(m.host.textContent!.includes("Your own address was cleared"), "origin cleared"));
    equal(world.posts.find((p) => p.path === "/api/crm-send/origin")!.body, { unsubscribe_origin: "https://cadence.example.com" }, "origin body");
    await m.unmount();
  }
  // Read-only viewers see the state but get no write controls.
  {
    const a = smtpRow("conn-a");
    const world = fresh({ connections: [a], binding: bindingFor(a) });
    const m = await mount(world, { operator: true, readOnly: true });
    await m.open("CRM · Acme");
    await m.settle(() => assert(m.host.querySelector("[data-state=live]"), "status visible"));
    assert(!m.button("Stop using"), "no sender write controls");
    await m.closeDrawer();
    await m.open("Unsubscribe links");
    await m.settle(() => assert(m.host.textContent!.includes("Every email"), "origin visible"));
    assert(!m.button("Save") && !m.host.querySelector("#email-origin"), "no origin write controls");
    await m.unmount();
  }
  // An account failure leaves app bindings and unsubscribe policy independent.
  {
    const a = smtpRow("conn-a");
    const world = fresh({ failConnectionsOnce: true, binding: bindingFor(a), reads: [] });
    const m = await mount(world);
    await m.settle(() => {
      assert(m.button("Retry"), "account read offers Retry");
      assert(m.rowFor("CRM · Acme")?.textContent?.includes("conn-a@example.com"), "binding remains truthful");
      assert(m.rowFor("Unsubscribe links"), "unsubscribe remains available");
    });
    assert(!m.button("Add email") && !m.host.textContent!.includes("No email accounts connected"), "failed list is not empty");
    await m.open("CRM · Acme");
    await m.settle(() => assert(m.host.querySelector("[data-state=live]"), "drawer still shows binding"));
    assert(!m.host.querySelector('[role="combobox"]') && !m.button("Switch to this sender"), "failed list cannot assign");
    const appReads = world.reads!.filter((p) => p === "/api/app-installations").length;
    await m.click(m.button("Retry"));
    await m.settle(() => assert(m.button("Add email"), "account retry recovers"));
    equal(world.reads!.filter((p) => p === "/api/app-installations").length, appReads, "account retry does not reload apps");
    assert(m.host.querySelector("[data-state=live]"), "account retry preserves the open sender drawer");
    await m.unmount();
  }
  // A partial response retains known account management but cannot authorize adds or assignment.
  {
    const a = smtpRow("conn-a");
    const world = fresh({ connectionsPartial: true, binding: bindingFor(a), reads: [] });
    const m = await mount(world);
    await m.settle(() => assert(m.host.querySelector("[data-state=connections-partial]"), "partial list notice"));
    assert(m.host.textContent!.includes("Some connected accounts couldn't be loaded"), "partial notice explains the list");
    assert(m.host.querySelector('a[href="/settings/connections/email/account/conn-a"]'), "known account remains manageable");
    assert(!m.button("Add email") && !m.host.textContent!.includes("No email accounts connected"), "partial list does not imply empty or permit Add");
    await m.open("CRM · Acme");
    await m.settle(() => assert(m.host.querySelector("[data-state=live]"), "known binding is displayed"));
    assert(!m.host.querySelector('[role="combobox"]') && !m.button("Switch to this sender"), "partial list cannot assign");
    world.connectionsPartial = false;
    const appReads = world.reads!.filter((p) => p === "/api/app-installations").length;
    await m.click(m.button("Retry"));
    await m.settle(() => assert(m.host.querySelector('[role="combobox"]'), "complete retry restores sender controls"));
    equal(world.reads!.filter((p) => p === "/api/app-installations").length, appReads, "partial retry only reloads accounts");
    await m.unmount();
  }
  {
    const m = await mount(fresh({ connections: [], connectionsPartial: true }));
    await m.settle(() => assert(m.host.querySelector("[data-state=connections-partial]"), "empty partial response"));
    assert(!m.host.textContent!.includes("No email accounts connected") && !m.button("Add email"), "empty partial response does not assert empty");
    await m.open("CRM · Acme");
    await m.settle(() => assert(m.host.querySelector("[data-state=unbound]"), "unbound status remains visible"));
    assert(!m.host.textContent!.includes("No email sender is available.") && !m.button("Use this sender"), "partial response cannot imply no sender or bind");
    await m.unmount();
  }
  // Context discovery can fail independently without hiding account management.
  {
    const world = fresh({ failContextsOnce: true, reads: [] });
    const m = await mount(world);
    await m.settle(() => {
      assert(m.button("Retry"), "context discovery offers Retry");
      assert(m.host.querySelector('a[href="/settings/connections/email/account/conn-a"]'), "account remains manageable");
      assert(m.button("Add email"), "account add remains available");
      assert(m.rowFor("Unsubscribe links"), "unsubscribe remains available");
    });
    assert(!m.host.querySelector("[data-state=no-apps]"), "failed discovery is not empty");
    const accountReads = world.reads!.filter((p) => p === "/api/connections").length;
    await m.click(m.button("Retry"));
    await m.settle(() => assert(m.rowFor("CRM · Acme"), "context retry recovers"));
    equal(world.reads!.filter((p) => p === "/api/connections").length, accountReads, "discovery retry does not reload accounts");
    await m.unmount();
  }
  // A failed sender read inside the drawer offers Retry too.
  {
    const a = smtpRow("conn-a");
    const world = fresh({ connections: [a], binding: bindingFor(a) });
    const m = await mount(world);
    await m.settle(() => assert(m.rowFor("CRM · Acme")?.textContent?.includes("conn-a@example.com"), "row read"));
    world.failShowOnce = true;
    await m.open("CRM · Acme");
    await m.settle(() => assert(m.button("Retry"), "the drawer offers Retry"));
    await m.click(m.button("Retry"));
    await m.settle(() => assert(m.host.querySelector("[data-state=live]"), "Retry recovers"));
    await m.unmount();
  }
  {
    const world = fresh({ failBindOnce: true });
    const m = await mount(world);
    await m.open("CRM · Acme");
    await m.settle(() => assert(m.button("Use this sender"), "binding can start"));
    await m.click(m.button("Use this sender"));
    await m.settle(() => assert(m.button("Retry"), "a refused write is recoverable in the same drawer"));
    await m.click(m.button("Retry"));
    await m.settle(() => assert(m.button("Use this sender"), "retry re-reads the binding and restores the action"));
    equal(world.posts.filter((post) => post.path.endsWith("/bind")).length, 1, "retry does not replay a write");
    await m.unmount();
  }
  // The two Email routes share a mounted page; returning still focuses App senders.
  {
    const m = await mount(fresh());
    await m.render(true);
    assert(document.activeElement?.id === "app-senders", "first deep link focuses App senders");
    await m.render(false);
    await m.settle(() => assert(m.button("Add email"), "Accounts loaded after leaving App senders"));
    m.button("Add email")?.focus();
    assert(document.activeElement?.textContent?.trim() === "Add email", "focus moved within Accounts");
    await m.render(true);
    assert(document.activeElement?.id === "app-senders", "returning deep link refocuses App senders");
    await m.unmount();
  }
  // A hosted workspace can manage its enrolled sender, not add a duplicate.
  {
    const m = await mount(fresh({
      hostedSmtp: true, connections: [smtpRow("conn-hosted", { account: "email" })],
    }), { operator: true, readOnly: false }, true);
    await m.settle(() => assert(m.host.querySelector('a[href="/settings/connections/email/account/conn-hosted"]'), "existing hosted account can be managed"));
    assert(!m.button("Add email"), "the single hosted SMTP account cannot be replaced through Add email");
    await m.unmount();
  }
  // A hosted board's links always use its own address: there is no setting to show.
  {
    const m = await mount(fresh(), { operator: true, readOnly: false }, true);
    await m.settle(() => assert(m.rowFor("CRM · Acme"), "the app row renders"));
    assert(!m.rowFor("Unsubscribe links"), "no unsubscribe setting on a hosted board");
    await m.unmount();
  }
  {
    const world = fresh({ hostedSmtp: true, connections: [smtpRow("conn-a"), { ...smtpRow("unrelated"), smtp_sender: false, smtp: null }], reads: [], connectionQueries: [] });
    const m = await mount(world, { operator: true, readOnly: false }, true);
    await m.settle(() => assert(m.host.querySelector('a[href="/settings/connections/email/account/conn-a"]'), "SMTP account renders"));
    assert(!m.host.textContent?.includes("unrelated"), "legacy non-SMTP rows are filtered");
    equal(world.connectionQueries, ["?scope=smtp"], "email requests SMTP scope");
    assert(!world.reads?.includes("/api/connection-providers"), "email overview does not load providers");
    await m.unmount();
  }
  {
    const a = smtpRow("conn-a");
    const world = fresh({ connections: [a], binding: bindingFor(a), failShowOnce: true });
    const m = await mount(world, { operator: true, readOnly: false }, true, "conn-a");
    await m.settle(() => assert(m.host.textContent?.includes("usage may be incomplete"), "binding failure is explicit"));
    await m.click(m.button("Disconnect account"));
    assert(m.host.textContent?.includes("Disconnect is refused while an app still uses this account"), "disconnect warns when usage is unknown");
    assert(!world.posts.some((post) => post.path.endsWith("/revoke")), "opening confirmation does not disconnect");
    await m.click(m.button("Cancel"));
    await m.click(m.button("Retry"));
    await m.settle(() => assert(m.host.textContent?.includes("CRM · Acme"), "binding retry restores the affected app"));
    assert(!m.button("Disconnect account")?.disabled, "operator can open the disconnect explanation");
    await m.click(m.button("Disconnect account"));
    assert(m.host.textContent?.includes("Stop using this account in CRM · Acme before disconnecting"), "hosted confirmation explains the bound-account refusal");
    assert(!m.button("Disconnect") && m.host.querySelector('a[href="/settings/connections/email/senders"]'), "the drawer links to the remedy instead of offering an impossible write");
    await m.unmount();
  }
  {
    const world = fresh({ connectionsPartial: true });
    const m = await mount(world, { operator: true, readOnly: false }, true, "conn-a");
    await m.settle(() => assert(m.host.textContent?.includes("could not be fully confirmed"), "partial account warning"));
    assert(m.button("Update credentials")?.disabled, "partial account cannot rotate");
    assert(m.button("Disconnect account")?.disabled, "partial account cannot disconnect");
    await m.unmount();
  }
  // Legacy peers may ignore scope and return unrelated rows; Manage reads the
  // same scoped resource and reports only bindings for this exact account.
  {
    const { connections: [a] } = JSON.parse(require("node:fs").readFileSync("tests/fixtures/smtp-connections.json", "utf8"));
    const other = smtpRow("conn-b");
    const world = fresh({
      hostedSmtp: true, connections: [a, other, { ...smtpRow("unrelated"), smtp_sender: false, smtp: null }],
      contexts: [{ id: "ctx-a", state: "active", config: { label: "Acme" } }, { id: "ctx-b", state: "active", config: { label: "Billing" } }],
      bindings: { "ctx-a": bindingFor(a), "ctx-b": bindingFor(other) },
      reads: [], connectionQueries: [],
    });
    const m = await mount(world, { operator: true, readOnly: false }, true);
    await m.settle(() => assert(m.host.textContent?.includes("hello@example.test"), "Email renders the scoped account"));
    await m.showAccount(a.id);
    await m.settle(() => assert(m.host.textContent?.includes("CRM · Acme"), "exact matching app is shown"));
    assert(!m.host.textContent?.includes("CRM · Billing"), "other account's binding is excluded");
    assert(m.host.querySelector("details:not([open])"), "advanced settings start collapsed");
    assert(!m.host.querySelector("input[type=password]"), "account overview displays no password");
    equal(world.connectionQueries, ["?scope=smtp"], "manage requests only SMTP metadata");
    assert(!world.reads?.includes("/api/connection-providers"), "manage never loads the provider directory");
    await m.unmount();
  }
  {
    const world = fresh({ connectionQueries: [] });
    const m = await mount(world);
    await m.settle(() => assert(m.host.querySelector('a[href="/settings/connections/email/account/conn-a"]'), "native Email opens without query parameters"));
    await m.showAccount("conn-a");
    await m.settle(() => assert(m.button("Update credentials") && !m.button("Update credentials")?.disabled, "native Manage enables credentials after its ordinary list read"));
    equal(world.connectionQueries, [""], "native Email and Manage reuse the native-compatible read");
    await m.unmount();
  }
  {
    const world = fresh({ hostedSmtp: true, denyConnections: true });
    const m = await mount(world, { operator: true, readOnly: false }, true, "conn-a");
    await m.settle(() => assert(m.host.textContent?.includes("Account access could not be confirmed"), "the server's refusal has its own state"));
    assert(!m.button("Update credentials") && !m.host.textContent?.includes("Email account not found"), "access denial neither exposes actions nor looks like a missing account");
    await m.unmount();
  }
  {
    const world = fresh({ hostedSmtp: true, connectionQueries: [] });
    const m = await mount(world, { operator: true, readOnly: false }, true, "conn-a");
    await m.settle(() => assert(m.button("Update credentials") && !m.button("Update credentials")?.disabled, "known account displays the update action"));
    world.failConnectionsOnce = true;
    await m.click(m.button("Update credentials"));
    await m.settle(() => assert(m.host.textContent?.includes("could not be fully confirmed"), "rotation needs a successful fresh account read"));
    assert(!m.host.querySelector("input[type=password]"), "the warm account is not a rotation baseline after a failed read");
    equal(world.connectionQueries, ["?scope=smtp", "?scope=smtp"], "only the explicit write action obtains a fresh baseline");
    world.connections = [smtpRow("conn-a", { revision: 2 })];
    await m.click(m.button("Retry"));
    await m.settle(() => assert(m.button("Update credentials") && !m.button("Update credentials")?.disabled, "retry restores account controls"));
    const latest = smtpRow("conn-a", { revision: 3 });
    latest.smtp.host = "smtp.current.example.test";
    world.connections = [latest];
    await m.click(m.button("Update credentials"));
    await m.settle(() => assert(m.host.querySelector("input[type=password]"), "a fresh account opens the credential drawer"));
    assert(m.host.querySelector('form input[placeholder="smtp.current.example.test"]'), "the form inherits the newly confirmed settings, not the warm snapshot");
    await m.unmount();
  }
  console.log("email sending checks passed");
}
void main();
