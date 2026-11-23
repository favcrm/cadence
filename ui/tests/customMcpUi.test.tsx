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
  const innerFetch = globalThis.fetch;
  globalThis.fetch = (async (input: unknown, init?: RequestInit) => {
    const url = String(input);
    const method = init?.method ?? "GET";
    if (url.endsWith("/api/connections/mcp") && method === "GET") return jsonRes({ connections: [returnedMcp] });
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
      return jsonRes({
        mode: "oauth_redirect", authorization_url: "https://auth.acme.example/authorize?state=xyz", connection_id: "conn_mcp_new_1",
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
  await act(async () => byText('[data-connection-policy] [role="radio"]', "Read only")!.click());
  await act(async () => byText('[data-connection-policy] button', "Save access settings")!.click());
  await settle(() => assert(document.querySelector('.custmcp-ok-banner')?.textContent?.includes("Access settings saved"), "green ok banner shown instead of warning"));

  await act(async () => (document.querySelector('[data-drawer="custom-mcp"] button[aria-label*="Close"]') as HTMLButtonElement).click());
  const closingDrawer = document.querySelector('[data-drawer="custom-mcp"][data-closing]');
  assert(closingDrawer, "inspect drawer enters closing state");
  await act(async () => { closingDrawer.dispatchEvent(new win.Event("transitionend", { bubbles: true })); });
  await settle(() => assert(!document.querySelector('[data-drawer="custom-mcp"]'), "inspect drawer closes"));

  await act(async () => byText("button", "+ Connect custom MCP server")!.click());
  await settle(() => assert(text().includes("1. Endpoint") && text().includes("2. Discovery & Consent") && text().includes("Auto MCP OAuth 2.1 + PKCE"), "3-step connect drawer opens"));

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
  await act(async () => {
    setNativeValue(document.querySelector('input[placeholder="Acme Billing MCP"]') as HTMLInputElement, "Acme OAuth");
    setNativeValue(document.querySelector('input[type="url"]') as HTMLInputElement, "https://mcp.acme.example/v1/mcp");
  });
  await act(async () => { document.querySelector("form")!.dispatchEvent(new win.Event("submit", { bubbles: true, cancelable: true })); });
  await settle(() => {
    const preview = document.querySelector("[data-mcp-discovery-preview]")?.textContent ?? "";
    assert(preview.includes("auth.acme.example") && preview.includes("Dynamic Client Registration") && preview.includes("Continue to OAuth sign-in"), "Step 2 discovery preview shown");
  });

  policyCalls.length = 0;
  await act(async () => { root.render(React.createElement(Connections, { viewer: { operator: false, readOnly: false }, hostedBoard: true })); });
  await settle(() => {
    assert(text().includes("Shared Custom MCP servers (read-only)") && !text().includes("+ Connect custom MCP server") && document.querySelectorAll('input[type="password"]').length === 0, "member view read-only");
  });
  await act(async () => (host.querySelector('[data-connections-group="custom-mcp"] button') as HTMLButtonElement).click());
  await settle(() => {
    const drawer = document.querySelector('[data-drawer="custom-mcp"]');
    assert(drawer && drawer.textContent?.includes("Read-only view") && drawer.querySelectorAll('.custmcp-mode-btn').length === 0, "member read-only drawer");
  });
  await act(async () => byText('[role="tab"]', "Policies")!.click());
  await settle(() => assert(document.querySelector('[data-mcp-policy-readonly]') && policyCalls.length === 0, "member Policies read-only with 0 /policy requests"));

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

  const brokerAnnotations = { readOnlyHint: true, destructiveHint: false };
  const readGuard = guardedMcpReadRequest.read_guard;
  const reviewedTool = {
    ...sampleMcp.tools[0],
    tool_name: guardedMcpReadRequest.tools[0].tool_name,
    description: "Search the catalog.",
    annotations: { readOnlyHint: true },
    tool_digest: readGuard.tool_digest,
    updated_at: readGuard.tool_updated_at,
  };
  returnedMcp = { ...sampleMcp, snapshot_revision: readGuard.snapshot_revision, tools: [
    reviewedTool,
    { ...sampleMcp.tools[0], tool_name: "execute_tool", description: "Broker", annotations: brokerAnnotations },
  ] };
  await openSetup("recovered-tab");
  await settle(() => assert(byText("button", "Continue in chat"), "shared receipt recovers the exact active connection"));
  await act(async () => byText("summary", "Review tool access")!.click());
  assert(!document.querySelector('[aria-label="Enable read access: execute_tool"]'), "broker has no inline read promotion after disclosure");
  await act(async () => (document.querySelector('[aria-label="Enable read access: catalog.search"]') as HTMLButtonElement).click());
  await settle(() => assert(!text().includes("No tools can run automatically"), "enable updates the verified tool status"));
  assert(JSON.stringify(toolWrites) === JSON.stringify([guardedMcpReadRequest]),
    "explicit enable matches the shared wire fixture for the reviewed pin");

  returnedMcp = { ...returnedMcp, tools: [reviewedTool] };
  rejectNextGuard = true;
  await openSetup("stale-guard");
  await settle(() => assert(byText("button", "Continue in chat"), "stale case shows reviewed tool"));
  await act(async () => byText("summary", "Review tool access")!.click());
  await act(async () => (document.querySelector('[aria-label="Enable read access: catalog.search"]') as HTMLButtonElement).click());
  await settle(() => assert(text().includes("Refresh and review again"), "stale guard requires review and reload"));
  assert(toolWrites.at(1) && !toolWrites.at(2) && JSON.stringify(toolWrites[0]) === JSON.stringify(toolWrites[1]),
    "stale response does not trigger an unguarded update");

  await openSetup("cancelled", "cancelled");
  assert(text().includes("was cancelled") && !byText("button", "Continue in chat"), "cancel is recoverable without claiming success");
  await openSetup("wrong-endpoint", "failed");
  await act(async () => byText("button", "Edit endpoint")!.click());
  assert(document.querySelector<HTMLInputElement>('input[type="url"]')?.readOnly === false, "failed setup can edit its endpoint");
  assert(text().includes("mcp:read"), "edited setup retains the requested OAuth scope");

  globalThis.fetch = innerFetch;
  await act(async () => root.unmount());
  console.log("customMcpUi: ok");
}

void main();
