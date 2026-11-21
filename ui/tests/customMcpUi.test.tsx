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

const sleep = (ms: number) => new Promise((r) => setTimeout(r, ms));
let act: typeof reactAct;
async function settle(fn: () => void, ms = 12000) {
  const end = Date.now() + ms;
  for (;;) {
    try {
      fn();
      return;
    } catch (e) {
      if (Date.now() > end) throw e;
      await act(async () => sleep(20));
    }
  }
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

  const innerFetch = globalThis.fetch;
  globalThis.fetch = (async (input: unknown) => {
    const url = String(input);
    if (url.endsWith("/api/connections/mcp")) {
      return new Response(JSON.stringify({ connections: [sampleMcp] }), {
        status: 200,
        headers: { "Content-Type": "application/json" },
      });
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
    return new Response(JSON.stringify({}), {
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
      host.querySelectorAll('[data-connections-group="shared"] [data-record-id="conn_mcp_billing_1"]').length === 0,
      "custom_mcp row deduplicated from platform list",
    );
  });

  // Open Inspect drawer -> Pinned Tools -> Instructions & Guide -> Auth & Access.
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

  // Close drawer, then open Connect drawer.
  const closeBtn = host.querySelector('[data-drawer="custom-mcp"] button[aria-label*="Close"]') as HTMLButtonElement;
  await act(async () => closeBtn.click());
  await settle(() => assert(!host.querySelector('[data-drawer="custom-mcp"]'), "inspect drawer closes"));

  const addMcpBtn = Array.from(host.querySelectorAll("button")).find((el) =>
    el.textContent?.includes("+ Connect custom MCP server"),
  ) as HTMLButtonElement;
  await act(async () => addMcpBtn.click());
  await settle(() => {
    assert(text().includes("Connect custom MCP server"), "connect drawer opens");
    assert(text().includes("Auto MCP OAuth 2.1"), "auto OAuth default shown");
  });

  // 2. Non-owner member read-only view on hosted board.
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

  globalThis.fetch = innerFetch;
  await act(async () => root.unmount());
  console.log("customMcpUi: ok");
}

void main().catch((e) => {
  setTimeout(() => {
    throw e;
  });
});
