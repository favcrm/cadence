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

function assert(cond: unknown, msg: string): asserts cond {
  if (!cond) throw new Error(`customMcpUi: ${msg}`);
}

let act: typeof reactAct;
// act drains React effects and their promise continuations without timers.
const drain = () => act(async () => { await Promise.resolve(); });
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
    id: "conn_mcp_billing_1",
    name: "Acme Billing",
    endpoint_url: "https://mcp.acme.example/v1/mcp",
    endpoint_host: "mcp.acme.example",
    auth_kind: "oauth",
    auth_hint: "OAuth 2.1",
    oauth_issuer: "https://auth.acme.example",
    oauth_scopes: ["mcp:read"],
    oauth_account: null,
    visibility: "shared",
    status: "active",
    snapshot_revision: 2,
    pending_revision: null,
    server_name: "acme-billing-mcp",
    server_version: "1.4.0",
    server_instructions_digest: "sha256:abc",
    has_server_instructions: true,
    instructions_drifted: false,
    has_agent_guide: true,
    agent_guide_source: "https://docs.acme.example/llms.txt",
    last_synced_at: 1_795_000_000_000,
    last_used_at: null,
    created_at: 1_795_000_000_000,
    updated_at: 1_795_000_000_000,
    tools_summary: { total: 1, read: 0, approval: 1, off: 0, drifted: 0 },
    tools: [
      {
        tool_name: "create_invoice",
        description: "Create a customer invoice draft.",
        input_schema: {
          type: "object",
          properties: { customer_id: { type: "string" } },
          required: ["customer_id"],
        },
        annotations: { readOnlyHint: false },
        tool_digest: "sha256:tool1",
        mode: "approval",
        drifted: false,
        pending_description: null,
        pending_tool_digest: null,
        updated_at: 1_795_000_000_000,
      },
    ],
    server_instructions: "Always include idempotency keys on invoice writes.",
    pending_server_instructions: null,
    agent_guide_md: "# Acme Billing Guide\n- Search customers before creating invoices.",
  };

  const platformRows = [
    {
      id: "conn_mcp_billing_1",
      provider: "custom_mcp",
      account: "mcp.acme.example",
      kind: "enrolled",
      revision: null,
      registration_digest: null,
      scopes: [],
      status: {
        adapter_registered: true,
        descriptor_available: true,
        custody_available: true,
        manifest_status: "matched",
        reviewed_pin: null,
        reported_pin: null,
        execution_authority: true,
        network_checked: false,
      },
    },
    {
      id: "conn_smtp_1",
      provider: "smtp",
      account: "ops@acme.example",
      kind: "enrolled",
      revision: 1,
      registration_digest: null,
      scopes: [],
      status: {
        adapter_registered: true,
        descriptor_available: true,
        custody_available: true,
        manifest_status: "approved",
        reviewed_pin: null,
        reported_pin: null,
        execution_authority: true,
        network_checked: false,
      },
    },
  ];

  const policyCalls: string[] = [];
  const innerFetch = globalThis.fetch;
  globalThis.fetch = (async (input: unknown, init?: RequestInit) => {
    const url = String(input);
    const method = init?.method ?? "GET";
    if (url.endsWith("/api/connections/mcp") && method === "GET") {
      return new Response(JSON.stringify({ connections: [sampleMcp] }), {
        status: 200,
        headers: { "Content-Type": "application/json" },
      });
    }
    if (url.endsWith("/api/connections/mcp") && method === "POST") {
      return new Response(
        JSON.stringify({
          mode: "oauth_redirect",
          authorization_url: "https://auth.acme.example/authorize?state=xyz",
          connection_id: "conn_mcp_new_1",
          discovery: {
            issuer: "https://auth.acme.example",
            authorization_host: "auth.acme.example",
            registration: "dcr",
            scopes: ["mcp:read"],
            resource_metadata_url: "https://mcp.acme.example/.well-known/oauth-protected-resource",
          },
        }),
        { status: 200, headers: { "Content-Type": "application/json" } },
      );
    }
    if (url.endsWith("/api/connections")) {
      return new Response(JSON.stringify({ connections: platformRows }), {
        status: 200,
        headers: { "Content-Type": "application/json" },
      });
    }
    if (url.endsWith("/api/connection-providers")) {
      return new Response(JSON.stringify({ providers: [] }), {
        status: 200,
        headers: { "Content-Type": "application/json" },
      });
    }
    if (url.endsWith("/api/agents")) {
      return new Response(JSON.stringify({ agents: [{ alias: "nova" }] }), {
        status: 200,
        headers: { "Content-Type": "application/json" },
      });
    }
    if (url.endsWith(`/api/connections/${sampleMcp.id}/policy`)) {
      policyCalls.push(`${method} ${url}`);
      return new Response(
        JSON.stringify({
          ok: true,
          policy: {
            connection_id: sampleMcp.id,
            provider: "custom_mcp",
            visibility: method === "POST" ? "personal" : "shared",
            owner_user_id: method === "POST" ? "usr_1" : null,
            is_default: false,
            write_mode: "ask_approval",
            allow_external_triggers: false,
            allowed_agents: ["nova"],
            policy_revision: 1,
            updated: 1,
            updated_at: 1,
          },
        }),
        { status: 200, headers: { "Content-Type": "application/json" } },
      );
    }
    return new Response(JSON.stringify({ ok: true }), {
      status: 200,
      headers: { "Content-Type": "application/json" },
    });
  }) as typeof fetch;

  const host = document.createElement("div");
  document.body.appendChild(host);
  const root = createRoot(host);
  const text = () => host.textContent ?? "";

  // 1. Owner view on hosted board.
  await act(async () => {
    root.render(
      React.createElement(Connections, {
        viewer: { operator: true, readOnly: false },
        hostedBoard: true,
      }),
    );
  });
  await settle(() => {
    assert(text().includes("+ Connect custom MCP server"), "owner sees connect MCP CTA");
    assert(
      host.querySelector('[data-connections-group="custom-mcp"]')?.textContent?.includes("Acme Billing"),
      "owner sees Custom MCP server row",
    );
    assert(
      host.querySelector('[data-connections-group="custom-mcp"]')?.textContent?.includes("1 tool") &&
        !host.querySelector('[data-connections-group="custom-mcp"]')?.textContent?.includes("1 tools"),
      "singular tool count rendered as '1 tool'",
    );
    assert(
      host.querySelectorAll('[data-connections-group="shared"] [data-record-id="conn_mcp_billing_1"]').length === 0,
      "custom_mcp row deduplicated from platform list",
    );
  });

  // Open Inspect drawer -> Pinned Tools -> Instructions & Guide -> Policies -> Auth & Access.
  const mcpBtn = host.querySelector('[data-connections-group="custom-mcp"] button') as HTMLButtonElement;
  assert(mcpBtn, "MCP row button exists");
  await act(async () => mcpBtn.click());
  await settle(() => {
    const drawer = host.querySelector('[data-drawer="custom-mcp"]');
    assert(drawer && drawer.textContent?.includes("create_invoice"), "inspect drawer shows pinned tool");
  });

  const guideTab = Array.from(host.querySelectorAll('[role="tab"]')).find((el) =>
    el.textContent?.includes("Instructions & Guide"),
  ) as HTMLButtonElement | undefined;
  assert(guideTab, "Instructions & Guide tab renders");
  await act(async () => guideTab.click());
  await settle(() => {
    assert(
      text().includes("Always include idempotency keys") &&
        text().includes("Search customers before creating invoices."),
      "guide tab shows server_instructions and agent_guide_md",
    );
  });

  const policiesTab = Array.from(host.querySelectorAll('[role="tab"]')).find((el) =>
    el.textContent?.includes("Policies"),
  ) as HTMLButtonElement | undefined;
  assert(policiesTab, "Policies tab renders");
  await act(async () => policiesTab.click());
  await settle(() => {
    assert(
      host.querySelector('[data-connection-policy]') !== null &&
        text().includes("Action safety") &&
        text().includes("nova"),
      "owner Policies tab shows connection policy & allowed agents",
    );
  });
  const readOnlyOpt = Array.from(host.querySelectorAll('[data-connection-policy] [role="radio"]')).find((el) =>
    el.textContent?.includes("Read only"),
  ) as HTMLButtonElement | undefined;
  assert(readOnlyOpt, "Read only policy radio exists");
  await act(async () => readOnlyOpt.click());
  const savePolicyBtn = Array.from(host.querySelectorAll('[data-connection-policy] button')).find((el) =>
    el.textContent?.includes("Save access settings"),
  ) as HTMLButtonElement | undefined;
  assert(savePolicyBtn, "Save access settings button renders after change");
  await act(async () => savePolicyBtn.click());
  await settle(() => {
    assert(host.querySelector('.custmcp-ok-banner')?.textContent?.includes("Access settings saved"), "green ok banner shown instead of warning");
  });

  // Close drawer, then open Connect drawer -> human-readable auth summary & 3-step OAuth discovery preview.
  const closeBtn = host.querySelector('[data-drawer="custom-mcp"] button[aria-label*="Close"]') as HTMLButtonElement;
  await act(async () => closeBtn.click());
  const closingDrawer = host.querySelector('[data-drawer="custom-mcp"][data-closing]');
  assert(closingDrawer, "inspect drawer enters closing state");
  await act(async () => {
    closingDrawer.dispatchEvent(new win.Event("transitionend", { bubbles: true }));
  });
  await settle(() => assert(!host.querySelector('[data-drawer="custom-mcp"]'), "inspect drawer closes"));

  const addMcpBtn = Array.from(host.querySelectorAll("button")).find((el) =>
    el.textContent?.includes("+ Connect custom MCP server"),
  ) as HTMLButtonElement;
  await act(async () => addMcpBtn.click());
  await settle(() => {
    assert(text().includes("Connect custom MCP server"), "connect drawer opens");
    assert(text().includes("1. Endpoint") && text().includes("2. Discovery & Consent"), "3-step connect indicator shown");
    assert(text().includes("Auto MCP OAuth 2.1 + PKCE"), "auto OAuth default shown");
  });

  // Selecting OAuth client ID displays human label in <summary>, never raw "oauth_static".
  const oauthStaticOpt = Array.from(host.querySelectorAll('[role="radio"]')).find((el) =>
    el.textContent?.includes("OAuth client ID"),
  ) as HTMLButtonElement;
  await act(async () => oauthStaticOpt.click());
  await settle(() => {
    const summary = host.querySelector('.custmcp-manual summary')?.textContent ?? "";
    assert(summary.includes("OAuth client ID") && !summary.includes("oauth_static"), "summary uses human label, not raw enum");
  });
  const autoOpt = Array.from(host.querySelectorAll('[role="radio"]')).find((el) =>
    el.textContent?.includes("Auto MCP OAuth 2.1"),
  ) as HTMLButtonElement;
  await act(async () => autoOpt.click());
  const nameInput = host.querySelector('input[placeholder="Acme Billing MCP"]') as HTMLInputElement;
  const urlInput = host.querySelector('input[type="url"]') as HTMLInputElement;
  const setNativeValue = (el: HTMLInputElement, val: string) => {
    const desc = Object.getOwnPropertyDescriptor(win.HTMLInputElement.prototype, "value")!;
    desc.set!.call(el, val);
    el.dispatchEvent(new win.Event("input", { bubbles: true }));
  };
  await act(async () => {
    setNativeValue(nameInput, "Acme OAuth");
    setNativeValue(urlInput, "https://mcp.acme.example/v1/mcp");
  });
  const form = host.querySelector("form") as HTMLFormElement;
  await act(async () => {
    form.dispatchEvent(new win.Event("submit", { bubbles: true, cancelable: true }));
  });
  await settle(() => {
    const preview = host.querySelector("[data-mcp-discovery-preview]");
    assert(
      preview &&
        preview.textContent?.includes("auth.acme.example") &&
        preview.textContent?.includes("Dynamic Client Registration") &&
        preview.textContent?.includes("Continue to OAuth sign-in"),
      "Step 2 RFC 9728 / DCR discovery preview shown before redirect",
    );
  });

  // 2. Non-owner member read-only view on hosted board (no policy mutations / fetches).
  policyCalls.length = 0;
  await act(async () => {
    root.render(
      React.createElement(Connections, {
        viewer: { operator: false, readOnly: false },
        hostedBoard: true,
      }),
    );
  });
  await settle(() => {
    assert(text().includes("Shared Custom MCP servers (read-only)"), "member sees shared MCP servers read-only");
    assert(!text().includes("+ Connect custom MCP server"), "member has no add MCP CTA");
    assert(host.querySelectorAll('input[type="password"]').length === 0, "member sees no password inputs");
  });

  const memberRow = host.querySelector('[data-connections-group="custom-mcp"] button') as HTMLButtonElement;
  await act(async () => memberRow.click());
  await settle(() => {
    const drawer = host.querySelector('[data-drawer="custom-mcp"]');
    assert(drawer && drawer.textContent?.includes("Read-only view"), "member opens read-only inspect drawer");
    assert(drawer.querySelectorAll('.custmcp-mode-btn').length === 0, "member has no tool mode mutation buttons");
  });
  const memberPoliciesTab = Array.from(host.querySelectorAll('[role="tab"]')).find((el) =>
    el.textContent?.includes("Policies"),
  ) as HTMLButtonElement;
  await act(async () => memberPoliciesTab.click());
  await settle(() => {
    assert(host.querySelector('[data-mcp-policy-readonly]') !== null, "member sees read-only Policies summary");
    assert(policyCalls.length === 0, "member never calls owner-only /policy endpoint");
  });

  // 3. ConnectCard shows "Continue reply" on ?aos_connect=connected&toolkit=custom_mcp return.
  const { ConnectCard } = require("../src/features/home/ConnectCard") as typeof import("../src/features/home/ConnectCard");
  win.history.replaceState(
    {},
    "",
    "/home?aos_connect=connected&toolkit=custom_mcp&connection_id=conn_mcp_billing_1",
  );
  await act(async () => {
    root.render(
      React.createElement(
        PageLocale,
        null,
        React.createElement(ConnectCard, {
          entry: {
            seq: 10,
            role: "agent",
            kind: "connect_ref",
            text: "Acme Billing OAuth expired; reconnect to continue.",
            message: "msg_1",
            payload: { v: 1, toolkit: "custom_mcp", toolkit_label: "Acme Billing" },
          },
        }),
      ),
    );
  });
  await settle(() => {
    const contBtn = host.querySelector('[data-continue-mcp="Acme Billing"]') as HTMLButtonElement | null;
    assert(contBtn && contBtn.textContent?.includes("Continue reply"), "ConnectCard renders Continue reply after OAuth callback return");
  });
  const contBtn = host.querySelector('[data-continue-mcp="Acme Billing"]') as HTMLButtonElement;
  await act(async () => contBtn.click());
  await settle(() => {
    assert(text().includes("Reply resumed"), "Continue reply sends turn and updates card to Reply resumed");
  });

  globalThis.fetch = innerFetch;
  await act(async () => root.unmount());
  console.log("customMcpUi: ok");
}

void main();
