/**
 * CAD-2556: Mounted HappyDOM acceptance check for Custom MCP UI in Settings > Connections.
 * Verifies:
 *  - Owner on hosted board sees "+ Connect custom MCP server", deduplicated Custom MCP list,
 *    Connect drawer (Auto OAuth + Manual Auth details), and 3-tab Inspect drawer.
 *  - Non-owner member on hosted board sees Shared Custom MCP servers read-only without any
 *    mutation controls, password inputs, or secrets.
 */
declare function require(name: string): any;
import type { act as reactAct } from "react";
import type { ThreadEntry } from "../src/features/home/thread";
import { guardedMcpReadRequest } from "./custom-mcp-read-guard.fixture";

function assert(cond: unknown, msg: string): asserts cond {
  if (!cond) throw new Error(`customMcpUi: ${msg}`);
}

let act: typeof reactAct;
// Allow asynchronous Web Crypto and React effects to complete.
const drain = () => act(async () => { await require("node:timers/promises").setTimeout(0); });
async function settle(fn: () => void) {
  for (let turn = 0; turn < 40; turn++) {
    await drain();
    try {
      fn();
      return;
    } catch {
      /* drain another microtask/effect turn */
    }
  }
  fn();
}

async function main() {
  const { Window } = require("happy-dom");
  const win = new Window({ url: "http://localhost/settings/connections" });
  for (const name of [
    "window",
    "document",
    "Node",
    "Element",
    "HTMLElement",
    "HTMLInputElement",
    "HTMLTextAreaElement",
    "HTMLSelectElement",
    "SVGElement",
    "navigator",
    "MutationObserver",
    "ResizeObserver",
    "Event",
    "MouseEvent",
    "KeyboardEvent",
    "location",
    "history",
    "sessionStorage",
  ]) {
    Object.defineProperty(globalThis, name, {
      value: name === "window" ? win : win[name],
      configurable: true,
      writable: true,
    });
  }
  for (const name of ["addEventListener", "removeEventListener"]) {
    Object.defineProperty(globalThis, name, { value: win[name].bind(win), configurable: true });
  }
  Object.defineProperty(globalThis, "IS_REACT_ACT_ENVIRONMENT", { value: true });
  Object.defineProperty(globalThis, "crypto", { value: require("node:crypto").webcrypto, configurable: true });

  const loader = require("module");
  const originalRequire = loader.prototype.require;
  loader.prototype.require = function (this: unknown, id: string) {
    if (id.endsWith(".css")) return {};
    if (id === "@hugeicons/core-free-icons") return new Proxy({}, { get: () => ({}) });
    if (id === "@hugeicons/react") return { HugeiconsIcon: () => null };
    return originalRequire.apply(this, arguments);
  };

  const React = require("react") as typeof import("react");
  act = React.act;
  const { createRoot } = require("react-dom/client") as typeof import("react-dom/client");
  const { default: ConnectionsView } = require("../src/features/settings/Connections") as typeof import("../src/features/settings/Connections");
  const { LocaleProvider: PageLocale } = require("../src/lib/locale") as typeof import("../src/lib/locale");
  const Connections = (props: import("react").ComponentProps<typeof ConnectionsView>) =>
    React.createElement(PageLocale, null, React.createElement(ConnectionsView, props));

  const sampleMcp = {
    id: "conn_mcp_billing_1", name: "Acme Billing", endpoint_url: "https://mcp.acme.example/v1/mcp", endpoint_host: "mcp.acme.example",
    auth_kind: "oauth", auth_hint: "OAuth 2.1", oauth_issuer: "https://auth.acme.example", oauth_scopes: ["mcp:read"], oauth_account: null,
    connected_by_you: true,
    visibility: "shared", status: "active", snapshot_revision: 2, pending_revision: null, server_name: "acme-billing-mcp", server_version: "1.4.0",
    server_instructions_digest: "sha256:abc", has_server_instructions: true, instructions_drifted: false, has_agent_guide: true,
    agent_guide_source: "https://docs.acme.example/llms.txt", last_synced_at: 1_795_000_000_000, last_used_at: null, created_at: 1_795_000_000_000, updated_at: 1_795_000_000_000,
    tools_summary: { total: 1, read: 0, approval: 1, off: 0, drifted: 0 },
    tools: [{
      tool_name: "create_invoice", description: "Create a customer invoice draft.",
      input_schema: { type: "object", properties: { customer_id: { type: "string" } }, required: ["customer_id"] },
      annotations: { readOnlyHint: false }, tool_digest: "sha256:tool1", mode: "approval", drifted: false,
      pending_description: null, pending_tool_digest: null, updated_at: 1_795_000_000_000,
    }],
    server_instructions: "Always include idempotency keys on invoice writes.", pending_server_instructions: null,
    agent_guide_md: "# Acme Billing Guide\n- Search customers before creating invoices.",
  };

  const statusObj = (manifest_status: string) => ({
    adapter_registered: true, descriptor_available: true, custody_available: true, manifest_status,
    reviewed_pin: null, reported_pin: null, execution_authority: true, network_checked: false,
  });
  const platformRows = [
    { id: "conn_mcp_billing_1", provider: "custom_mcp", account: "mcp.acme.example", kind: "enrolled", revision: null, registration_digest: null, scopes: [], status: statusObj("matched") },
    { id: "conn_smtp_1", provider: "smtp", account: "ops@acme.example", kind: "enrolled", revision: 1, registration_digest: null, scopes: [], status: statusObj("approved") },
  ];

  const jsonRes = (body: unknown) => new Response(JSON.stringify(body), { status: 200, headers: { "Content-Type": "application/json" } });
  const policyCalls: string[] = [];
  let returnedMcp = sampleMcp;
  const messageWrites: Array<Record<string, unknown>> = [];
  let messageMode: "ok" | "uncertain" | "deferred" = "ok";
  let resolveMessage: ((response: Response) => void) | undefined;
  let rejectNextGuard = false;
  const toolWrites: unknown[] = [];
  let standingProfile = false;
  let rejectStandingGrant = false;
  let mismatchStandingGrant = false;
  const creates: Array<Record<string, unknown>> = [];
  const catalogGrants: Array<Record<string, unknown>> = [];
  const innerFetch = globalThis.fetch;
  globalThis.fetch = (async (input: unknown, init?: RequestInit) => {
    const url = String(input);
    const method = init?.method ?? "GET";
    if (url.endsWith("/api/connections/mcp") && method === "GET") return jsonRes({
      connections: [returnedMcp], ...(standingProfile ? { standing_read_offers: [{ profile: "inventory_v2", endpoint_url: "https://inventory.example/", required_scopes: ["stock:read"], tool_name: "stock.lookup" }] } : {}),
    });
    if (url.endsWith(`/api/connections/mcp/${sampleMcp.id}/allow-catalog-reads`) && method === "POST") {
      const request = JSON.parse(String(init?.body));
      catalogGrants.push(request);
      if (rejectStandingGrant) {
        rejectStandingGrant = false;
        return new Response(JSON.stringify({ error: { code: "refused", message: "Access refused" } }), {
          status: 403, headers: { "Content-Type": "application/json" },
        });
      }
      if (mismatchStandingGrant) {
        mismatchStandingGrant = false;
        return jsonRes({ connection: { ...returnedMcp, standing_read_profile: "inventory_v2", standing_read_access: "allowed", oauth_scopes: ["wrong:scope"] } });
      }
      if (request.snapshot_revision !== returnedMcp.snapshot_revision)
        return new Response(JSON.stringify({ error: { code: "stale_revision", message: "Snapshot changed" } }), {
          status: 409, headers: { "Content-Type": "application/json" },
        });
      returnedMcp = { ...returnedMcp, visibility: "personal", standing_read_profile: "inventory_v2",
        standing_read_access: "allowed" } as typeof sampleMcp;
      return jsonRes({ connection: returnedMcp });
    }
    if (url.endsWith(`/api/connections/mcp/${sampleMcp.id}/tools`) && method === "POST") {
      const body = JSON.parse(String(init?.body));
      toolWrites.push(body);
      if (body.read_guard) {
        const selected = returnedMcp.tools.find((tool) => tool.tool_name === body.tools?.[0]?.tool_name);
        if (rejectNextGuard || body.tools.length !== 1 || body.tools[0].mode !== "read" ||
            body.read_guard.snapshot_revision !== returnedMcp.snapshot_revision ||
            body.read_guard.tool_digest !== selected?.tool_digest ||
            body.read_guard.tool_updated_at !== selected?.updated_at) {
          rejectNextGuard = false;
          return new Response(JSON.stringify({ error: { code: "stale_revision", message: "Snapshot changed" } }), {
            status: 409, headers: { "Content-Type": "application/json" },
          });
        }
      }
      returnedMcp = { ...returnedMcp, tools: returnedMcp.tools.map((tool) =>
        tool.tool_name === "catalog.search" ? { ...tool, mode: "read" } : tool) };
      return jsonRes({ connection: returnedMcp });
    }
    if (url.endsWith("/api/connections/mcp") && method === "POST") {
      const reqBody = JSON.parse(String(init?.body)) as Record<string, unknown>;
      creates.push(reqBody);
      if (standingProfile) return jsonRes({
        mode: "oauth_redirect", authorization_url: "https://inventory.example/authorize?scope=stock%3Aread",
        connection_id: sampleMcp.id,
      });
      const scopeParam = Array.isArray(reqBody.required_scopes) && reqBody.required_scopes.length > 0
        ? `&scope=${encodeURIComponent((reqBody.required_scopes as string[]).join(" "))}`
        : "";
      return jsonRes({
        mode: "oauth_redirect", authorization_url: `https://auth.acme.example/authorize?state=xyz${scopeParam}`, connection_id: "conn_mcp_new_1",
        discovery: { issuer: "https://auth.acme.example", authorization_host: "auth.acme.example", registration: "dcr", scopes: ["mcp:read"], resource_metadata_url: "https://mcp.acme.example/.well-known/oauth-protected-resource" },
      });
    }
    if (url.endsWith("/api/threads/master/messages") && method === "POST") {
      messageWrites.push(JSON.parse(String(init?.body)));
      if (messageMode === "uncertain") {
        messageMode = "ok";
        throw new Error("Response lost");
      }
      if (messageMode === "deferred") {
        return await new Promise<Response>((resolve) => { resolveMessage = resolve; });
      }
      return jsonRes({ ok: true });
    }
    if (url.endsWith("/api/connections")) return jsonRes({ connections: platformRows });
    if (url.endsWith("/api/connection-providers")) return jsonRes({ providers: [] });
    if (url.endsWith("/api/agents")) return jsonRes({ agents: [{ alias: "nova" }] });
    if (url.endsWith(`/api/connections/${sampleMcp.id}/policy`)) {
      policyCalls.push(`${method} ${url}`);
      return jsonRes({
        ok: true,
        policy: {
          connection_id: sampleMcp.id, provider: "custom_mcp", visibility: method === "POST" ? "personal" : "shared",
          owner_user_id: method === "POST" ? "usr_1" : null, is_default: false, write_mode: "ask_approval",
          allow_external_triggers: false, allowed_agents: ["nova"], policy_revision: 1, updated: 1, updated_at: 1,
        },
      });
    }
    return jsonRes({ ok: true });
  }) as typeof fetch;

  const host = document.createElement("div");
  document.body.appendChild(host);
  const root = createRoot(host);
  const text = () => document.body.textContent ?? "";
  const byText = (sel: string, txt: string) =>
    Array.from(document.querySelectorAll(sel)).find((el) => el.textContent?.includes(txt)) as HTMLButtonElement | undefined;

  await act(async () => { root.render(React.createElement(Connections, { viewer: { operator: true, readOnly: false }, hostedBoard: true })); });
  await settle(() => {
    assert(text().includes("+ Connect custom MCP server"), "owner sees connect MCP CTA");
    const rowText = host.querySelector('[data-connections-group="custom-mcp"]')?.textContent ?? "";
    assert(rowText.includes("Acme Billing") && rowText.includes("1 tool") && !rowText.includes("1 tools"), "owner sees deduplicated row with singular '1 tool'");
    assert(host.querySelectorAll('[data-connections-group="shared"] [data-record-id="conn_mcp_billing_1"]').length === 0, "custom_mcp row deduplicated");
  });

  await act(async () => (host.querySelector('[data-connections-group="custom-mcp"] button') as HTMLButtonElement).click());
  await settle(() => assert(document.querySelector('[data-drawer="custom-mcp"]')?.textContent?.includes("create_invoice"), "inspect drawer shows pinned tool"));

  await act(async () => byText('[role="tab"]', "Instructions & Guide")!.click());
  await settle(() => assert(text().includes("Always include idempotency keys") && text().includes("Search customers before creating invoices."), "guide tab shows server & agent guides"));

  await act(async () => byText('[role="tab"]', "Policies")!.click());
  await settle(() => assert(document.querySelector('[data-connection-policy]') && text().includes("Action safety") && text().includes("nova"), "owner Policies tab shows policy & allowed agents"));
  await act(async () => byText('[data-connection-policy] [role="radio"]', "Just me (Personal)")!.click());
  await settle(() => assert(!byText('[data-connection-policy] [role="checkbox"]', "nova"), "switching to Personal hides agent access checkboxes"));
  await act(async () => byText('[data-connection-policy] [role="radio"]', "Shared with workspace")!.click());
  await act(async () => byText('[data-connection-policy] [role="radio"]', "Read only")!.click());
  await act(async () => byText('[data-connection-policy] button', "Save access settings")!.click());
  await settle(() => assert(document.querySelector('.custmcp-ok-banner')?.textContent?.includes("Access settings saved"), "green ok banner shown instead of warning"));

  // P2: Promoting personal -> shared shows pin reset warning banner in Policies tab.
  returnedMcp = { ...sampleMcp, visibility: "personal", connected_by_you: true };
  await act(async () => { root.render(React.createElement(Connections, { viewer: { operator: true, readOnly: false }, hostedBoard: true })); });
  await settle(() => assert(host.querySelector('[data-connections-group="custom-mcp"] button'), "personal connection listed"));
  await act(async () => (host.querySelector('[data-connections-group="custom-mcp"] button') as HTMLButtonElement).click());
  await settle(() => assert(document.querySelector('[data-drawer="custom-mcp"]'), "drawer opens"));
  await act(async () => byText('[role="tab"]', "Policies")!.click());
  await settle(() => assert(document.querySelector('[data-connection-policy]'), "policy tab ready"));
  await act(async () => byText('[data-connection-policy] [role="radio"]', "Shared with workspace")!.click());
  await settle(() => assert(text().includes("Promoting to Shared resets any write tools set to Always allow back to Ask first"), "P2 promote warning shown"));
  returnedMcp = { ...sampleMcp, visibility: "shared", connected_by_you: true };

  await act(async () => (document.querySelector('[data-drawer="custom-mcp"] button[aria-label*="Close"]') as HTMLButtonElement).click());
  const closingDrawer = document.querySelector('[data-drawer="custom-mcp"][data-closing]');
  assert(closingDrawer, "inspect drawer enters closing state");
  await act(async () => { closingDrawer.dispatchEvent(new win.Event("transitionend", { bubbles: true })); });
  await settle(() => assert(!document.querySelector('[data-drawer="custom-mcp"]'), "inspect drawer closes"));

  const redirectedUrls: string[] = [];
  const origAssign = win.location.assign?.bind(win.location);
  Object.defineProperty(win.location, "assign", {
    configurable: true,
    value: (nextUrl: string) => { redirectedUrls.push(nextUrl); },
  });

  await act(async () => byText("button", "+ Connect custom MCP server")!.click());
  await settle(() => assert(text().includes("1. Endpoint") && text().includes("2. Discovery & Consent") && text().includes("Auto MCP OAuth 2.1 + PKCE"), "3-step connect drawer opens"));
  assert(byText('[role="radio"][aria-checked="true"]', "Just me (Personal)") !== undefined, "connect drawer defaults to Personal");

  await act(async () => byText('[role="radio"]', "OAuth client ID")!.click());
  await settle(() => {
    const summary = document.querySelector('.custmcp-manual summary')?.textContent ?? "";
    assert(summary.includes("OAuth client ID") && !summary.includes("oauth_static"), "summary uses human label, not raw enum");
  });
  await act(async () => byText('[role="radio"]', "Auto MCP OAuth 2.1")!.click());
  const setNativeValue = (el: HTMLInputElement, val: string) => {
    Object.getOwnPropertyDescriptor(win.HTMLInputElement.prototype, "value")!.set!.call(el, val);
    el.dispatchEvent(new win.Event("input", { bubbles: true }));
  };
  // Same-site OAuth (mcp.acme.example -> auth.acme.example) redirects directly without stopping at Step 2.
  await act(async () => {
    setNativeValue(document.querySelector('input[placeholder="Acme Billing MCP"]') as HTMLInputElement, "Acme OAuth");
    setNativeValue(document.querySelector('input[type="url"]') as HTMLInputElement, "https://mcp.acme.example/v1/mcp");
  });
  await act(async () => { document.querySelector("form")!.dispatchEvent(new win.Event("submit", { bubbles: true, cancelable: true })); });
  await settle(() => {
    assert(redirectedUrls.some((u) => u.includes("auth.acme.example/authorize")), "same-site OAuth redirects immediately");
    assert(!document.querySelector("[data-mcp-discovery-preview]"), "same-site OAuth skips Step 2 confirmation");
  });
  assert(creates.at(-1)?.visibility === "personal", "Settings connect defaults visibility to personal");

  // Cross-site OAuth (mcp.other.example -> auth.acme.example) stops at Step 2 confirmation.
  await act(async () => {
    setNativeValue(document.querySelector('input[type="url"]') as HTMLInputElement, "https://mcp.other.example/v1/mcp");
  });
  await act(async () => { document.querySelector("form")!.dispatchEvent(new win.Event("submit", { bubbles: true, cancelable: true })); });
  await settle(() => {
    const preview = document.querySelector("[data-mcp-discovery-preview]")?.textContent ?? "";
    assert(preview.includes("auth.acme.example") && preview.includes("Dynamic Client Registration") && preview.includes("Continue to OAuth sign-in"), "Step 2 discovery preview shown for cross-site OAuth");
  });

  // Member read-only on shared server & unauthenticated readOnly gate.
  policyCalls.length = 0;
  await act(async () => { root.render(React.createElement(Connections, { viewer: { operator: false, readOnly: false }, hostedBoard: true })); });
  await settle(() => {
    assert(text().includes("+ Connect custom MCP server"), "signed-in member sees + Connect custom MCP server");
  });
  await act(async () => byText("button", "+ Connect custom MCP server")!.click());
  await settle(() => {
    const radios = Array.from(document.querySelectorAll('[role="dialog"] [role="radio"]')).map((el) => el.textContent ?? "");
    assert(radios.some((r) => r.includes("Personal")) && !radios.some((r) => r.includes("Shared")), "member connect drawer only offers Personal");
  });
  await act(async () => (document.querySelector('[role="dialog"] button[aria-label*="Close"]') as HTMLButtonElement).click());
  const closingConnect = document.querySelector('[role="dialog"][data-closing]');
  if (closingConnect) {
    await act(async () => { closingConnect.dispatchEvent(new win.Event("transitionend", { bubbles: true })); });
  }
  await settle(() => assert(host.querySelector('[data-connections-group="custom-mcp"] button'), "shared MCP listed for member"));
  await act(async () => (host.querySelector('[data-connections-group="custom-mcp"] button') as HTMLButtonElement).click());
  await settle(() => {
    const drawer = document.querySelector('[data-drawer="custom-mcp"]');
    assert(drawer && drawer.textContent?.includes("Read-only view") && drawer.querySelectorAll('.custmcp-mode-btn').length === 0, "member read-only on shared server");
  });
  await act(async () => byText('[role="tab"]', "Policies")!.click());
  await settle(() => assert(document.querySelector('[data-mcp-policy-readonly]') && policyCalls.length === 0, "member Policies read-only with 0 /policy requests"));

  // Member CAN edit tools on their own personal MCP connection.
  returnedMcp = { ...sampleMcp, visibility: "personal", connected_by_you: true };
  await act(async () => { root.render(React.createElement(Connections, { key: "member-personal", viewer: { operator: false, readOnly: false }, hostedBoard: true })); });
  await settle(() => assert(host.querySelector('[data-connections-group="custom-mcp"] button'), "member sees personal connection"));
  await act(async () => (host.querySelector('[data-connections-group="custom-mcp"] button') as HTMLButtonElement).click());
  await settle(() => assert(document.querySelector('[data-drawer="custom-mcp"] .custmcp-mode-btn'), "member can edit tools on their own personal server"));
  returnedMcp = sampleMcp;

  // Read-only unauthenticated viewer stays strictly read-only.
  await act(async () => { root.render(React.createElement(Connections, { viewer: { operator: false, readOnly: true }, hostedBoard: true })); });
  await settle(() => {
    assert(text().includes("Shared Custom MCP servers (read-only)") && !text().includes("+ Connect custom MCP server") && document.querySelectorAll('input[type="password"]').length === 0, "readOnly viewer read-only");
  });
  if (origAssign) {
    Object.defineProperty(win.location, "assign", { configurable: true, value: origAssign });
  }

  const { ConnectCard } = require("../src/features/home/ConnectCard") as typeof import("../src/features/home/ConnectCard");
  win.history.replaceState({}, "", "/home?aos_connect=connected&toolkit=custom_mcp&connection_id=conn_mcp_billing_1");
  await act(async () => {
    root.render(React.createElement(PageLocale, null, React.createElement(ConnectCard, {
      entry: { seq: 10, role: "agent", kind: "connect_ref", text: "Acme Billing OAuth expired; reconnect to continue.", message: "msg_1", payload: { v: 1, toolkit: "custom_mcp", toolkit_label: "Acme Billing" } },
    })));
  });
  await settle(() => assert(host.querySelector('[data-continue-mcp="Acme Billing"]')?.textContent?.includes("Continue reply"), "ConnectCard renders Continue reply"));
  await act(async () => (host.querySelector('[data-continue-mcp="Acme Billing"]') as HTMLButtonElement).click());
  await settle(() => assert(text().includes("Reply resumed"), "Continue reply updates card to Reply resumed"));

  const { ImageConversation } = require("../src/features/canvas/imageConversation") as typeof import("../src/features/canvas/imageConversation");
  const setupEntry = {
    seq: 20, role: "agent", kind: "connect_ref", message: "msg_setup", text: "Connect Acme Billing",
    payload: { v: 2, toolkit: "custom_mcp", toolkit_label: sampleMcp.name,
      endpoint_url: sampleMcp.endpoint_url, required_scopes: ["mcp:read"] },
  } satisfies ThreadEntry;
  const receiptKey = "cadence:mcp-setup:thread-20:20";
  async function openSetup(key: string, outcome = "connected", conversation = "thread-20", callbackNonce?: string) {
    const stored = win.localStorage.getItem(`cadence:mcp-setup:${conversation}:20`) ??
      win.sessionStorage.getItem(`cadence:mcp-setup:${conversation}:20`);
    const nonce = callbackNonce ?? stored?.split("\n")[2];
    win.history.replaceState({}, "", `/home?mcp_setup=20&aos_connect=${outcome}&toolkit=custom_mcp&connection_id=${sampleMcp.id}${nonce ? `&mcp_attempt=${nonce}` : ""}`);
    await act(async () => {
      root.render(React.createElement(PageLocale, null, React.createElement(ImageConversation, {
        value: conversation,
      }, React.createElement(ConnectCard, { key, entry: setupEntry }))));
    });
  }

  win.sessionStorage.setItem(receiptKey, sampleMcp.id);
  await openSetup("same-tab");
  await settle(() => assert(byText("button", "Continue in chat"), "active grant can continue with zero read pins"));
  assert(text().includes("No tools can run automatically"), "tool availability is separate from OAuth");
  await act(async () => byText("button", "Continue in chat")!.click());
  await settle(() => assert(text().includes("Reply resumed"), "chat resumes without a tool permission edit"));
  assert(toolWrites.length === 0, "continue does not change tool permissions");
  assert(win.sessionStorage.getItem(receiptKey) === null, "successful continue clears the old same-tab receipt");

  const attemptReceipt = `${sampleMcp.id}\n${sampleMcp.endpoint_url}\nattempt-one`;
  win.localStorage.setItem(receiptKey, attemptReceipt);
  messageWrites.length = 0;
  messageMode = "uncertain";
  await openSetup("uncertain-response");
  await settle(() => assert(byText("button", "Continue in chat"), "uncertain send has verified connection"));
  await act(async () => byText("button", "Continue in chat")!.click());
  await settle(() => assert(text().includes("Response lost"), "uncertain send remains retryable"));
  assert(win.localStorage.getItem(receiptKey) === attemptReceipt, "uncertain send retains its receipt");
  await openSetup("uncertain-other-tab");
  await settle(() => assert(byText("button", "Continue in chat"), "another tab recovers the same attempt"));
  await act(async () => byText("button", "Continue in chat")!.click());
  await settle(() => assert(text().includes("Reply resumed"), "same attempt can retry"));
  assert(messageWrites.length === 2 && JSON.stringify(messageWrites[0]) === JSON.stringify(messageWrites[1]),
    "uncertain retry sends exactly the same envelope");
  assert(typeof messageWrites[0].message === "string" && /^mcp-setup-[0-9a-f]{64}$/.test(messageWrites[0].message as string) &&
    messageWrites[0].conversation === "thread-20", "retry uses one bounded stable message ID in the original conversation");

  win.localStorage.setItem(receiptKey, attemptReceipt);
  messageWrites.length = 0;
  messageMode = "deferred";
  await openSetup("delayed-response");
  await settle(() => assert(byText("button", "Continue in chat"), "delayed send has verified connection"));
  await act(async () => byText("button", "Continue in chat")!.click());
  await settle(() => assert(resolveMessage && messageWrites.length === 1, "send is in flight"));
  const newerReceipt = `${sampleMcp.id}\n${sampleMcp.endpoint_url}\nattempt-two`;
  win.localStorage.setItem(receiptKey, newerReceipt);
  win.history.replaceState({}, "", "/home?mcp_setup=21&aos_connect=connected&toolkit=custom_mcp&connection_id=another#new");
  await act(async () => { resolveMessage!(jsonRes({ ok: true })); });
  assert(win.localStorage.getItem(receiptKey) === newerReceipt, "late completion does not consume the newer attempt");
  assert(win.location.search.includes("mcp_setup=21") && win.location.hash === "#new", "late completion does not navigate over a newer return");
  messageMode = "ok";
  resolveMessage = undefined;

  win.localStorage.setItem(receiptKey, attemptReceipt);
  messageWrites.length = 0;
  messageMode = "deferred";
  await openSetup("navigated-response");
  await settle(() => assert(byText("button", "Continue in chat"), "navigated send has verified connection"));
  await act(async () => byText("button", "Continue in chat")!.click());
  await settle(() => assert(resolveMessage && messageWrites.length === 1, "navigated send is in flight"));
  win.history.replaceState({}, "", "/settings/connections?mcp_focus=other#new");
  await act(async () => { resolveMessage!(jsonRes({ ok: true })); });
  assert(win.location.pathname === "/settings/connections" && win.location.search === "?mcp_focus=other" &&
    win.location.hash === "#new", "late completion preserves navigation even without a newer receipt");
  assert(win.localStorage.getItem(receiptKey) === null, "known success consumes the matching attempt");
  messageMode = "ok";
  resolveMessage = undefined;

  win.localStorage.setItem(receiptKey, attemptReceipt);
  messageWrites.length = 0;
  messageMode = "deferred";
  await openSetup("different-conversation-query");
  await settle(() => assert(byText("button", "Continue in chat"), "query navigation send has verified connection"));
  await act(async () => byText("button", "Continue in chat")!.click());
  await settle(() => assert(resolveMessage && messageWrites.length === 1, "query navigation send is in flight"));
  const otherConversation = win.location.search + "&conversation=another";
  win.history.replaceState({}, "", `/home${otherConversation}#new`);
  await act(async () => { resolveMessage!(jsonRes({ ok: true })); });
  assert(win.location.search === otherConversation && win.location.hash === "#new", "late completion preserves another conversation");
  messageMode = "ok";
  resolveMessage = undefined;

  win.localStorage.setItem(receiptKey, attemptReceipt);
  messageWrites.length = 0;
  messageMode = "deferred";
  await openSetup("unrelated-query");
  await settle(() => assert(byText("button", "Continue in chat"), "same callback has verified connection"));
  await act(async () => byText("button", "Continue in chat")!.click());
  await settle(() => assert(resolveMessage && messageWrites.length === 1, "same-route send is in flight"));
  const unrelated = "&mcp_focus=other&aos_publish=pending";
  win.history.replaceState({}, "", `/home${win.location.search}${unrelated}#new`);
  await act(async () => { resolveMessage!(jsonRes({ ok: true })); });
  const kept = new URLSearchParams(win.location.search);
  assert(kept.get("mcp_focus") === "other" && kept.get("aos_publish") === "pending" &&
    !kept.has("mcp_setup") && !kept.has("mcp_attempt") && win.location.hash === "#new",
  "cleanup removes only the matched callback and preserves unrelated integration state");
  messageMode = "ok";
  resolveMessage = undefined;

  win.localStorage.setItem(receiptKey, newerReceipt);
  await openSetup("old-callback", "connected", "thread-20", "attempt-one");
  assert(!byText("button", "Continue in chat"), "old callback cannot adopt a newer nonce for the same connection");
  assert(win.localStorage.getItem(receiptKey) === newerReceipt, "old callback retains the newer receipt");

  win.localStorage.removeItem(receiptKey);
  await openSetup("edit-before-connection", "failed");
  await act(async () => byText("button", "Edit endpoint")!.click());
  assert(document.querySelector<HTMLInputElement>('input[type="url"]')?.readOnly === false, "endpoint can be edited before a connection exists");
  await act(async () => root.render(React.createElement("div")));


  win.localStorage.setItem(receiptKey, `${sampleMcp.id}\n${sampleMcp.endpoint_url}`);
  await openSetup("different-thread", "connected", "another-thread");
  assert(!byText("button", "Continue in chat"), "return cannot resume another conversation");

  for (const [key, bad] of Object.entries({
    owner: { ...sampleMcp, connected_by_you: false },
    path: { ...sampleMcp, endpoint_url: "https://mcp.acme.example/" },
    scopes: { ...sampleMcp, oauth_scopes: ["mcp:read", "mcp:write"] },
  })) {
    returnedMcp = bad;
    await openSetup(key);
    await settle(() => assert(text().includes("could not verify"), `cannot resume with a different ${key}`));
    assert(!byText("button", "Continue in chat"), "unverified return has no continuation");
  }

  const reviewedTool = {
    ...sampleMcp.tools[0],
    tool_name: guardedMcpReadRequest.tools[0].tool_name,
    description: "Search the catalog.",
    annotations: { readOnlyHint: true },
    mode: "read" as const,
    tool_digest: guardedMcpReadRequest.read_guard.tool_digest,
    updated_at: guardedMcpReadRequest.read_guard.tool_updated_at,
  };
  returnedMcp = { ...sampleMcp, snapshot_revision: guardedMcpReadRequest.read_guard.snapshot_revision, tools: [reviewedTool] };
  win.localStorage.setItem(receiptKey, attemptReceipt);
  messageWrites.length = 0;
  await openSetup("auto-resume-readable");
  await settle(() => assert(messageWrites.length === 1 && text().includes("Reply resumed"), "active readable connection auto-resumes chat without manual review accordion"));
  assert(!document.body.textContent?.includes("Review tool access"), "Review tool access accordion removed");

  // Compact Confirm Modal in chat ConnectCard: Personal default, owner vs member scope, same-site redirect vs cross-site confirm.
  const { HireBlock, WriteGate } = require("../src/features/auth/WriteGate") as typeof import("../src/features/auth/WriteGate");
  win.localStorage.removeItem(receiptKey);
  win.history.replaceState({}, "", "/home");
  standingProfile = false;
  const modalRedirects: string[] = [];
  Object.defineProperty(win.location, "assign", {
    configurable: true,
    value: (nextUrl: string) => { modalRedirects.push(nextUrl); },
  });
  await act(async () => {
    root.render(React.createElement(PageLocale, null, React.createElement(WriteGate.Provider, { value: null }, React.createElement(HireBlock.Provider, { value: "Owner only" }, React.createElement(ImageConversation, { value: "thread-20" }, React.createElement(ConnectCard, { key: "member-modal", entry: setupEntry }))))));
  });
  await settle(() => assert(byText("button", "Connect Acme Billing"), "member sees Connect button in chat"));
  await act(async () => byText("button", "Connect Acme Billing")!.click());
  await settle(() => assert(document.querySelector("[data-mcp-confirm-modal]") && text().includes("Read-only tools run automatically"), "member opens compact Confirm Modal"));
  assert(!byText("[data-mcp-confirm-modal] [role=\"radio\"]", "Shared with company"), "member modal hides Shared with company");
  await act(async () => byText("[data-mcp-confirm-modal] button", "Confirm & Connect")!.click());
  await settle(() => assert(modalRedirects.some((u) => u.includes("auth.acme.example/authorize")), "member modal auto-redirects on same-site OAuth"));
  assert(creates.at(-1)?.visibility === "personal", "member modal sends visibility: personal");

  // Owner modal shows Shared toggle and cross-site confirmation.
  const crossSiteEntry = {
    ...setupEntry, seq: 22,
    payload: { v: 2, toolkit: "custom_mcp", toolkit_label: "External MCP", endpoint_url: "https://mcp.other-vendor.example/v1/mcp", required_scopes: ["mcp:read"] },
  } satisfies ThreadEntry;
  await act(async () => {
    root.render(React.createElement(PageLocale, null, React.createElement(WriteGate.Provider, { value: null }, React.createElement(HireBlock.Provider, { value: null }, React.createElement(ImageConversation, { value: "thread-22" }, React.createElement(ConnectCard, { key: "owner-modal", entry: crossSiteEntry }))))));
  });
  await settle(() => assert(byText("button", "Connect External MCP"), "owner sees Connect External MCP button"));
  await act(async () => byText("button", "Connect External MCP")!.click());
  await settle(() => assert(byText('[data-mcp-confirm-modal] [role="radio"]', "Shared with company"), "owner sees Shared with company option"));
  assert(byText('[data-mcp-confirm-modal] [role="radio"][aria-checked="true"]', "Personal (just me)") !== undefined, "owner modal defaults to Personal");
  await act(async () => byText("[data-mcp-confirm-modal] button", "Confirm & Connect")!.click());
  await settle(() => assert(document.querySelector("[data-mcp-cross-site-confirm]") && text().includes("auth.acme.example"), "cross-site OAuth shows inline confirmation inside modal"));
  await act(async () => byText("[data-mcp-confirm-modal] button", "✕")!.click());
  if (origAssign) {
    Object.defineProperty(win.location, "assign", { configurable: true, value: origAssign });
  }
  win.localStorage.removeItem("cadence:mcp-setup:thread-22:22");
  win.localStorage.setItem(receiptKey, attemptReceipt);
  returnedMcp = sampleMcp;

  await openSetup("cancelled", "cancelled");
  assert(text().includes("was cancelled") && !byText("button", "Continue in chat"), "cancel is recoverable without claiming success");
  await openSetup("wrong-endpoint", "failed");
  await act(async () => byText("button", "Edit endpoint")!.click());
  assert(document.querySelector<HTMLInputElement>('input[type="url"]')?.readOnly === false, "failed setup can edit its endpoint");
  assert(text().includes("mcp:read"), "edited setup retains the requested OAuth scope");

  const inventoryEndpoint = "https://inventory.example/";
  const inventoryEntry = {
    ...setupEntry, seq: 21, text: "Find inventory items",
    payload: { v: 2, toolkit: "custom_mcp", toolkit_label: "Inventory", endpoint_url: inventoryEndpoint, required_scopes: ["stock:read"] },
  } satisfies ThreadEntry;
  const inventoryKey = "cadence:mcp-setup:thread-21:21";
  const renderInventory = async (key: string) => act(async () => root.render(
    React.createElement(PageLocale, null, React.createElement(ImageConversation, { value: "thread-21" },
      React.createElement(ConnectCard, { key, entry: inventoryEntry }))),
  ));
  win.localStorage.removeItem(inventoryKey);
  win.history.replaceState({}, "", "/home");
  standingProfile = false;
  await renderInventory("older");
  await settle(() => assert(byText("button", "Connect Inventory") && !byText("button", "Connect Inventory for read access"), "older server keeps generic setup"));
  standingProfile = true;
  await renderInventory("preset");
  await settle(() => assert(byText("button", "Connect Inventory for read access"), "trusted server exposes read consent"));
  await act(async () => byText("button", "Connect Inventory for read access")!.click());
  await settle(() => assert(byText("a", "Continue to OAuth sign-in"), "consent starts validated OAuth handoff"));
  const created = creates.at(-1)!;
  assert(created.standing_read_profile === "inventory_v2" && created.visibility === "personal" &&
    JSON.stringify(created.required_scopes) === '["stock:read"]', "preset sends offered personal scope");
  const receipt = win.localStorage.getItem(inventoryKey)!;
  const nonce = receipt.split("\n")[2];
  assert(nonce && receipt.endsWith("\ninventory_v2"), "handoff preserves a selected nonce and preset");
  returnedMcp = { ...sampleMcp, name: "Inventory", endpoint_url: inventoryEndpoint, oauth_scopes: ["stock:read"],
    standing_read_profile: "inventory_v2", standing_read_access: "allowed" } as typeof sampleMcp;
  messageWrites.length = 0;
  const callbackUrl = `/home?mcp_setup=21&aos_connect=connected&toolkit=custom_mcp&connection_id=${sampleMcp.id}&mcp_attempt=${nonce}`;
  win.history.replaceState({}, "", callbackUrl);
  await renderInventory("returned");
  await settle(() => assert(text().includes("Connected · Read access") && messageWrites.length === 1, "matched approved callback auto resumes"));
  assert(!byText("button", "Continue in chat") && !byText("button", "Enable read access"), "preset does not expose generic promotion or continuation");
  assert(messageWrites.at(0)?.conversation === "thread-21" && /^mcp-setup-[0-9a-f]{64}$/.test(String(messageWrites.at(0)?.message)), "resume targets the original conversation with deterministic identity");
  assert(JSON.stringify(messageWrites[0]).includes("stock.lookup"), "resume names only the offered tool");
  assert(toolWrites.slice().length === 0, "preset does not write tool modes");
  await renderInventory("remounted");
  assert(messageWrites.length === 1, "remount does not send again");
  await settle(() => assert(text().includes("Connected · Read access"), "already allowed connection stays visible after reload"));
  assert(!byText("button", "Connect Inventory for read access"), "already authorized owners do not start OAuth again");
  win.localStorage.setItem(inventoryKey, receipt);
  win.history.replaceState({}, "", callbackUrl);
  messageWrites.length = 0;
  messageMode = "uncertain";
  await renderInventory("uncertain-preset");
  await settle(() => assert(byText("button", "Resume"), "failed automatic send offers recovery"));
  await act(async () => byText("button", "Resume")!.click());
  await settle(() => assert(text().includes("Connected · Read access") && messageWrites.length === 2, "resume recovers uncertain send"));
  assert(JSON.stringify(messageWrites[0]) === JSON.stringify(messageWrites[1]), "recovery reuses the identical message envelope");
  messageWrites.length = 0;
  win.localStorage.setItem(inventoryKey, receipt);
  win.history.replaceState({}, "", callbackUrl.replace(nonce, "wrong"));
  await renderInventory("forged");
  await drain();
  assert(messageWrites.length === 0, "wrong callback nonce never resumes");
  win.history.replaceState({}, "", callbackUrl.replace(`&mcp_attempt=${nonce}`, ""));
  await renderInventory("empty");
  await drain();
  assert(messageWrites.length === 0, "empty callback nonce never resumes");
  win.localStorage.removeItem(inventoryKey);
  win.history.replaceState({}, "", "/home");
  const createsBeforeLegacyGrant = creates.length;
  returnedMcp = { ...sampleMcp, name: "Inventory", endpoint_url: inventoryEndpoint, oauth_scopes: ["stock:read"],
    standing_read_access: "confirm" } as typeof sampleMcp;
  await renderInventory("legacy-confirm");
  await settle(() => assert(byText("button", "Allow Atlas read access"), "legacy owner can grant access in chat"));
  rejectStandingGrant = true;
  await act(async () => byText("button", "Allow Atlas read access")!.click());
  await settle(() => assert(document.querySelector('[role="alert"]')?.textContent?.includes("Could not complete the Custom MCP request. Try again."), "refusal is shown"));
  assert(messageWrites.length === 0, "refusal never resumes");
  mismatchStandingGrant = true;
  await act(async () => byText("button", "Allow Atlas read access")!.click());
  await settle(() => assert(document.querySelector('[role="alert"]')?.textContent?.includes("Could not complete the Custom MCP request. Try again."), "mismatched grant is refused"));
  assert(messageWrites.length === 0, "mismatched grant never resumes");
  await act(async () => byText("button", "Allow Atlas read access")!.click());
  await settle(() => assert(text().includes("Connected · Read access") && messageWrites.length === 1, "grant resumes the waiting conversation"));
  assert(catalogGrants.length === 3 && catalogGrants.every((grant) => grant.snapshot_revision === sampleMcp.snapshot_revision), "uses guarded existing-connection action");
  assert(creates.length === createsBeforeLegacyGrant && win.location.pathname === "/home", "keeps the saved login and stays in chat");
  assert(messageWrites.at(0)?.conversation === "thread-21" && /^mcp-setup-[0-9a-f]{64}$/.test(String(messageWrites.at(0)?.message)), "grant resumes only the original conversation");
  await renderInventory("legacy-remount");
  await settle(() => assert(text().includes("Connected · Read access"), "granted access persists after remount"));
  assert(messageWrites.slice().length === 1 && !byText("button", "Allow Atlas read access"), "grant does not ask or resume again");
  messageWrites.length = 0;
  win.localStorage.setItem(inventoryKey, receipt);
  standingProfile = false;
  win.history.replaceState({}, "", callbackUrl);
  await renderInventory("missing-profile");
  await settle(() => assert(text().includes("could not verify"), "in-flight preset fails closed on older server"));
  assert(messageWrites.length === 0, "missing trusted wrapper never resumes");

  // CAD-2759 / AOS-738: Last checked metadata + per-tool drift accept preserves unrelated mode drafts.
  const acceptPayloads: Array<Record<string, unknown>> = [];
  let driftedConn: Record<string, unknown> = {
    ...sampleMcp,
    status: "needs_review",
    pending_revision: 3,
    instructions_drifted: true,
    pending_server_instructions: "Updated instructions",
    last_checked_at: 1_795_000_050_000,
    last_check_outcome: "changes_found",
    tools_summary: { total: 3, read: 0, approval: 3, off: 0, drifted: 2 },
    tools: [
      {
        ...sampleMcp.tools[0],
        drifted: true,
        pending_change: "changed",
        pending_description: "Create an invoice draft with currency.",
        pending_tool_digest: "sha256:tool1_v2",
      },
      {
        tool_name: "legacy_export",
        description: "Old export.",
        input_schema: { type: "object", properties: {} },
        annotations: { readOnlyHint: true },
        tool_digest: "sha256:old",
        mode: "approval",
        drifted: true,
        pending_change: "removed",
        pending_description: null,
        pending_tool_digest: "__removed__",
        updated_at: 1_795_000_000_000,
      },
      {
        tool_name: "list_customers",
        description: "List customers.",
        input_schema: { type: "object", properties: {} },
        annotations: { readOnlyHint: true },
        tool_digest: "sha256:cust",
        mode: "approval",
        drifted: false,
        pending_description: null,
        pending_tool_digest: null,
        updated_at: 1_795_000_000_000,
      },
    ],
  };
  globalThis.fetch = (async (input: unknown, init?: RequestInit) => {
    const url = String(input);
    const method = init?.method ?? "GET";
    if (url.endsWith("/api/connections/mcp") && method === "GET") return jsonRes({ connections: [driftedConn] });
    if (url.endsWith(`/api/connections/mcp/${sampleMcp.id}/accept-drift`) && method === "POST") {
      const body = JSON.parse(String(init?.body ?? "{}")) as Record<string, unknown>;
      acceptPayloads.push(body);
      driftedConn = {
        ...driftedConn,
        snapshot_revision: 3,
        tools_summary: { total: 3, read: 0, approval: 3, off: 0, drifted: 1 },
        tools: (driftedConn.tools as Array<Record<string, unknown>>).map((pin) =>
          pin.tool_name === "create_invoice"
            ? { ...pin, drifted: false, pending_change: null, pending_description: null }
            : pin,
        ),
      };
      return jsonRes({ connection: driftedConn });
    }
    if (url.endsWith("/api/connections")) return jsonRes({ connections: platformRows });
    if (url.endsWith("/api/connection-providers")) return jsonRes({ providers: [] });
    if (url.endsWith("/api/agents")) return jsonRes({ agents: [{ alias: "nova" }] });
    return jsonRes({ ok: true });
  }) as typeof fetch;

  await act(async () => { root.render(React.createElement(Connections, { viewer: { operator: true, readOnly: false }, hostedBoard: true })); });
  await settle(() => assert(text().includes("Drift review"), "shows drifted server row"));
  await act(async () => (host.querySelector('[data-connections-group="custom-mcp"] button') as HTMLButtonElement).click());
  await settle(() => {
    const drawer = document.querySelector('[data-drawer="custom-mcp"]')?.textContent ?? "";
    assert(drawer.includes("Last checked") && drawer.includes("Changes found"), "shows Last checked & outcome chip");
    assert(drawer.includes("Changed") && drawer.includes("Removed upstream"), "shows Changed and Removed upstream chips");
  });
  await act(async () => {
    const listGroup = document.querySelector('[role="radiogroup"][aria-label^="list_customers"]');
    const offBtn = Array.from(listGroup?.querySelectorAll("button") ?? []).find((b) => b.textContent === "Off") as HTMLButtonElement | undefined;
    offBtn?.click();
  });
  await act(async () => (document.querySelector('button[aria-label="Accept create_invoice"]') as HTMLButtonElement).click());
  await settle(() => {
    assert(acceptPayloads.length === 1 && JSON.stringify(acceptPayloads[0]) === JSON.stringify({ tool_names: ["create_invoice"] }), "per-tool accept sends tool_names");
    assert(document.querySelector('[data-drawer="custom-mcp"]')?.textContent?.includes("Accepted change for create_invoice"), "shows partial accept feedback");
    const listGroup = document.querySelector('[role="radiogroup"][aria-label^="list_customers"]');
    const offBtn = Array.from(listGroup?.querySelectorAll("button") ?? []).find((b) => b.textContent === "Off");
    assert(offBtn?.getAttribute("aria-checked") === "true", "unrelated dirty mode draft preserved across partial accept");
  });

  globalThis.fetch = innerFetch;
  await act(async () => root.unmount());
  console.log("customMcpUi: ok");
}

void main();
