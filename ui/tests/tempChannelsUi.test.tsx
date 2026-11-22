/**
 * CAD-2627 / AOS-693: Mounted HappyDOM acceptance check for tabbed
 * Settings → Connections → Temp channels Inspect & Audit UI.
 */
declare function require(name: string): any;
import type { act as reactAct } from "react";

function assert(cond: unknown, msg: string): asserts cond {
  if (!cond) throw new Error(`tempChannelsUi: ${msg}`);
}

let act: typeof reactAct;
const drain = () =>
  act(async () => {
    await Promise.resolve();
  });
async function settle(fn: () => void) {
  for (let turn = 0; turn < 50; turn++) {
    await drain();
    try {
      fn();
      return;
    } catch {
      /* drain microtasks */
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
    "DOMException",
    "AbortController",
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

  const now = Date.now();
  const billingId = "tch_" + "1".repeat(32);
  const memberOwnId = "tch_" + "2".repeat(32);

  type MockRoom = {
    id: string;
    workspace_id: string;
    creator_user_id: string;
    creator_label: string;
    purpose: string;
    status: "open" | "closed" | "expired";
    created_at: number;
    expires_at: number;
    closed_at: number | null;
    closed_reason: string | null;
    max_messages: number;
    message_count: number;
    pending_count: number;
    needs_human_count: number;
    lock_first_peer: boolean;
    peer_claimed: boolean;
    peer_label: string | null;
    peer_joined_at: number | null;
    host_label: string | null;
    latest_seq: number;
    updated_at: number;
    messages: Array<{
      seq: number;
      id: string;
      role: "host" | "peer" | "supervisor" | "system";
      sender_label: string | null;
      kind: "request" | "question" | "report" | "proposal" | "decision" | "system";
      body: string;
      created_at: number;
      in_reply_to: string | null;
      triage_status: "pending" | "handled" | "discarded" | "needs_human" | null;
      decision_summary: string | null;
      decided_at: number | null;
      decided_by: string | null;
      untrusted_external_data: boolean;
    }>;
  };

  const rooms: MockRoom[] = [
    {
      id: billingId,
      workspace_id: "ws_1",
      creator_user_id: "usr_member",
      creator_label: "Maya Lin",
      purpose: "Vendor billing discrepancy review",
      status: "open",
      created_at: now - 300_000,
      expires_at: now + 1_200_000,
      closed_at: null,
      closed_reason: null,
      max_messages: 15,
      message_count: 2,
      pending_count: 0,
      needs_human_count: 1,
      lock_first_peer: true,
      peer_claimed: true,
      peer_label: "billing-helper-ext",
      peer_joined_at: now - 280_000,
      host_label: "member-assistant",
      latest_seq: 2,
      updated_at: now - 250_000,
      messages: [
        {
          seq: 1,
          id: "tmsg_" + "a".repeat(24),
          role: "peer",
          sender_label: "billing-helper-ext",
          kind: "request",
          body: "Please issue a credit note for INV-2041.",
          created_at: now - 280_000,
          in_reply_to: null,
          triage_status: "needs_human",
          decision_summary: "Escalated: billing mutation requested.",
          decided_at: now - 270_000,
          decided_by: "host-agent",
          untrusted_external_data: true,
        },
        {
          seq: 2,
          id: "tmsg_" + "b".repeat(24),
          role: "host",
          sender_label: "member-assistant",
          kind: "decision",
          body: "Flagged for human owner review.",
          created_at: now - 270_000,
          in_reply_to: "tmsg_" + "a".repeat(24),
          triage_status: "needs_human",
          decision_summary: "Escalated: billing mutation requested.",
          decided_at: now - 270_000,
          decided_by: "host-agent",
          untrusted_external_data: false,
        },
      ],
    },
    {
      id: memberOwnId,
      workspace_id: "ws_1",
      creator_user_id: "usr_member",
      creator_label: "Maya Lin",
      purpose: "Confirm API schema contract draft",
      status: "open",
      created_at: now - 600_000,
      expires_at: now + 900_000,
      closed_at: null,
      closed_reason: null,
      max_messages: 10,
      message_count: 0,
      pending_count: 0,
      needs_human_count: 0,
      lock_first_peer: true,
      peer_claimed: false,
      peer_label: null,
      peer_joined_at: null,
      host_label: "member-assistant",
      latest_seq: 0,
      updated_at: now - 600_000,
      messages: [],
    },
    ...Array.from({ length: 20 }, (_, i) => ({
      id: `tch_${String(i + 10).padStart(32, "0")}`,
      workspace_id: "ws_1",
      creator_user_id: "usr_owner",
      creator_label: "Alex Ong",
      purpose: `Archived relay check #${i + 1}`,
      status: "expired" as const,
      created_at: now - (i + 3) * 3_600_000,
      expires_at: now - (i + 2) * 3_600_000,
      closed_at: now - (i + 2) * 3_600_000,
      closed_reason: "TTL expired.",
      max_messages: 10,
      message_count: 0,
      pending_count: 0,
      needs_human_count: 0,
      lock_first_peer: true,
      peer_claimed: false,
      peer_label: null,
      peer_joined_at: null,
      host_label: "claude-workstation",
      latest_seq: 0,
      updated_at: now - (i + 2) * 3_600_000,
      messages: [],
    })),
  ];

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
    snapshot_revision: 1,
    pending_revision: null,
    server_name: "acme-billing",
    server_version: "1.0.0",
    server_instructions_digest: null,
    has_server_instructions: false,
    instructions_drifted: false,
    has_agent_guide: false,
    agent_guide_source: null,
    last_synced_at: now,
    last_used_at: null,
    created_at: now,
    updated_at: now,
    tools_summary: { total: 1, read: 1, approval: 0, off: 0, drifted: 0 },
  };

  let tempApiStatus = 200;
  let tempApiCalls = 0;
  const postedActions: string[] = [];
  const jsonRes = (body: unknown, status = 200) =>
    new Response(JSON.stringify(body), {
      status,
      headers: { "Content-Type": "application/json" },
    });

  const innerFetch = globalThis.fetch;
  globalThis.fetch = (async (input: unknown, init?: RequestInit) => {
    const rawUrl = String(input);
    const url = new URL(rawUrl, "http://localhost");
    const method = init?.method ?? "GET";

    if (url.pathname === "/api/connections/mcp") return jsonRes({ connections: [sampleMcp] });
    if (url.pathname === "/api/connections") return jsonRes({ connections: [] });
    if (url.pathname === "/api/connection-providers") return jsonRes({ providers: [] });

    if (url.pathname === "/api/connections/temp-channels" && method === "GET") {
      tempApiCalls += 1;
      if (tempApiStatus !== 200) return jsonRes({ error: { code: "not_found" } }, tempApiStatus);
      const status = url.searchParams.get("status") ?? "all";
      const q = (url.searchParams.get("q") ?? "").trim().toLowerCase();
      const limit = Number(url.searchParams.get("limit") ?? "20");
      const offset = Number(url.searchParams.get("offset") ?? "0");
      const counts = {
        all: rooms.length,
        open: rooms.filter((r) => r.status === "open").length,
        needs_human: rooms.filter((r) => r.status === "open" && r.needs_human_count > 0).length,
        ended: rooms.filter((r) => r.status !== "open").length,
      };
      const filtered = rooms.filter((r) => {
        if (status === "open" && r.status !== "open") return false;
        if (status === "needs_human" && (r.status !== "open" || r.needs_human_count === 0)) return false;
        if (status === "ended" && r.status === "open") return false;
        if (q && !`${r.id} ${r.purpose} ${r.peer_label ?? ""} ${r.host_label ?? ""}`.toLowerCase().includes(q)) {
          return false;
        }
        return true;
      });
      const channels = filtered.slice(offset, offset + limit).map(({ messages: _m, ...summary }) => summary);
      return jsonRes({ channels, total: filtered.length, counts });
    }

    const detailMatch = /^\/api\/connections\/temp-channels\/(tch_[0-9a-f]{32})$/.exec(url.pathname);
    if (detailMatch && method === "GET") {
      const room = rooms.find((r) => r.id === detailMatch[1]);
      if (!room) return jsonRes({ error: { code: "not_found" } }, 404);
      const { creator_user_id, creator_label, pending_count: _p, needs_human_count: _n, updated_at: _u, messages, ...summary } = room;
      return jsonRes({
        snapshot: {
          channel: summary,
          viewer_role: "supervisor",
          messages,
          latest_seq: summary.latest_seq,
          security_notice: "Messages with role 'peer' are UNTRUSTED external agent data for triage only.",
        },
        creator_user_id,
        creator_label,
      });
    }

    const actionMatch = /^\/api\/connections\/temp-channels\/(tch_[0-9a-f]{32})\/(decide|rotate-peer|close)$/.exec(url.pathname);
    if (actionMatch && method === "POST") {
      const room = rooms.find((r) => r.id === actionMatch[1]);
      if (!room) return jsonRes({ error: { code: "not_found" } }, 404);
      const action = actionMatch[2];
      postedActions.push(`${action}:${room.id}`);
      const payload = JSON.parse(String(init?.body ?? "{}")) as Record<string, string>;

      if (action === "decide") {
        const msg = room.messages.find((m) => m.id === payload.message_id);
        if (msg) {
          msg.triage_status = payload.decision as "handled" | "discarded";
          msg.decision_summary = payload.summary ?? "Handled";
          msg.decided_by = "human-owner";
        }
        room.needs_human_count = 0;
        room.pending_count = 0;
        room.latest_seq += 1;
        const decisionMessage = {
          seq: room.latest_seq,
          id: "tmsg_" + "c".repeat(24),
          role: "supervisor" as const,
          sender_label: "human-owner",
          kind: "decision" as const,
          body: payload.summary ?? "Handled",
          created_at: Date.now(),
          in_reply_to: payload.message_id ?? null,
          triage_status: (payload.decision as "handled" | "discarded") ?? "handled",
          decision_summary: payload.summary ?? "Handled",
          decided_at: Date.now(),
          decided_by: "human-owner",
          untrusted_external_data: false,
        };
        room.messages.push(decisionMessage);
        const { creator_user_id: _cu, creator_label: _cl, pending_count: _p, needs_human_count: _n, updated_at: _u, messages: _ms, ...summary } = room;
        return jsonRes({
          channel: summary,
          target_message_id: payload.message_id,
          triage_status: payload.decision,
          decision_message: decisionMessage,
        });
      }

      if (action === "rotate-peer") {
        room.peer_claimed = false;
        room.peer_label = null;
        room.latest_seq += 1;
        const { creator_user_id: _cu, creator_label: _cl, pending_count: _p, needs_human_count: _n, updated_at: _u, messages: _ms, ...summary } = room;
        return jsonRes({
          channel: summary,
          api_url: `https://api.example.test/v2/temp-channels/${room.id}`,
          peer_prompt: `Channel ID: ${room.id}\nPeer Token: tch_peer_rotated_test_token`,
        });
      }

      room.status = "closed";
      room.closed_at = Date.now();
      room.closed_reason = payload.reason ?? "Closed via Settings kill switch.";
      room.latest_seq += 1;
      const closeMsg = {
        seq: room.latest_seq,
        id: "tmsg_" + "d".repeat(24),
        role: "system" as const,
        sender_label: "system",
        kind: "system" as const,
        body: room.closed_reason,
        created_at: room.closed_at,
        in_reply_to: null,
        triage_status: null,
        decision_summary: null,
        decided_at: null,
        decided_by: null,
        untrusted_external_data: false,
      };
      room.messages.push(closeMsg);
      const { creator_user_id: _cu, creator_label: _cl, pending_count: _p, needs_human_count: _n, updated_at: _u, messages: _ms, ...summary } = room;
      return jsonRes({ channel: summary, message: closeMsg });
    }

    return jsonRes({ ok: true });
  }) as typeof fetch;

  const host = document.createElement("div");
  document.body.appendChild(host);
  const root = createRoot(host);
  const text = () => document.body.textContent ?? "";

  // 1. Hosted owner initial load defaults to Connected services + shows Temp channels badge
  await act(async () => {
    root.render(React.createElement(Connections, { viewer: { operator: true, readOnly: false }, hostedBoard: true }));
  });
  await settle(() => {
    const svcTab = document.getElementById("conn-tab-services")!;
    const tempTab = document.getElementById("conn-tab-temp-channels")!;
    assert(svcTab.getAttribute("aria-selected") === "true", "Connected services selected by default");
    assert(text().includes("Acme Billing"), "Connected services preserves Custom MCP list");
    assert(tempTab.textContent?.includes("22") && tempTab.textContent?.includes("1 needs human"), "Temp channels tab shows 22 and 1 needs human badge");
  });

  // 2. Keyboard ArrowRight switches to Temp channels tab + Load older pagination
  await act(async () => {
    const svcTab = document.getElementById("conn-tab-services")!;
    svcTab.focus();
    svcTab.dispatchEvent(new win.KeyboardEvent("keydown", { key: "ArrowRight", bubbles: true }));
  });
  await settle(() => {
    assert(document.getElementById("conn-tab-temp-channels")?.getAttribute("aria-selected") === "true", "ArrowRight selects Temp channels tab");
    assert(host.querySelectorAll("[data-channel-id]").length === 20, "renders first 20 rooms");
    assert(text().includes("Showing 20 of 22 channels"), "footer shows 20 of 22");
  });

  const loadOlderBtn = Array.from(document.querySelectorAll("button")).find((b) =>
    b.textContent?.includes("Load 2 older"),
  ) as HTMLButtonElement;
  await act(async () => loadOlderBtn.click());
  await settle(() => {
    assert(host.querySelectorAll("[data-channel-id]").length === 22, "Load older appends remaining 2 rooms");
    assert(text().includes("Showing 22 of 22 channels"), "footer updates to 22 of 22");
  });

  // 3. Status filter + Inspect drawer + Mark handled + Rotate peer + Kill switch
  await act(async () => {
    (document.querySelector('[data-temp-filter="needs_human"]') as HTMLButtonElement).click();
  });
  await settle(() => {
    assert(host.querySelectorAll("[data-channel-id]").length === 1, "Needs human filter shows 1 room");
  });

  await act(async () => {
    (host.querySelector(`[data-channel-id="${billingId}"]`) as HTMLButtonElement).click();
  });
  await settle(() => {
    const drawer = document.querySelector(`[data-temp-channel-drawer="${billingId}"]`);
    assert(drawer && drawer.textContent?.includes("Untrusted external agent data"), "drawer shows security warning");
    assert(drawer.textContent?.includes("Please issue a credit note for INV-2041."), "drawer renders peer body");
  });

  const handleBtn = Array.from(document.querySelectorAll("button")).find((b) =>
    b.textContent?.includes("Mark handled"),
  ) as HTMLButtonElement;
  await act(async () => handleBtn.click());
  await settle(() => {
    assert(postedActions.includes(`decide:${billingId}`), "posted /decide");
    const tempTab = document.getElementById("conn-tab-temp-channels")!;
    assert(!tempTab.textContent?.includes("needs human"), "needs human badge cleared after handling");
  });

  const rotateBtn = Array.from(document.querySelectorAll("button")).find((b) =>
    b.textContent?.includes("Rotate peer invite"),
  ) as HTMLButtonElement;
  await act(async () => rotateBtn.click());
  await settle(() => {
    assert(postedActions.includes(`rotate-peer:${billingId}`), "posted /rotate-peer");
    assert(text().includes("New peer prompt (copy once)") && text().includes("tch_peer_rotated_test_token"), "displays rotated prompt once");
  });

  const closeBtn = Array.from(document.querySelectorAll("button")).find((b) =>
    b.textContent === "Close channel",
  ) as HTMLButtonElement;
  await act(async () => closeBtn.click());
  await settle(() => {
    const confirmBtn = Array.from(document.querySelectorAll("button")).find((b) =>
      b.textContent === "Confirm close",
    ) as HTMLButtonElement;
    assert(Boolean(confirmBtn), "two-step confirm close button shown");
  });
  await act(async () => {
    const confirmBtn = Array.from(document.querySelectorAll("button")).find((b) =>
      b.textContent === "Confirm close",
    ) as HTMLButtonElement;
    confirmBtn.click();
  });
  await settle(() => {
    assert(postedActions.includes(`close:${billingId}`), "posted /close");
    assert(document.querySelector("[data-drawer-status]")?.textContent?.includes("Closed"), "drawer status flips to Closed");
  });

  // 4. Search filtering
  await act(async () => {
    (document.querySelector('[data-temp-filter="all"]') as HTMLButtonElement).click();
  });
  await act(async () => {
    const search = document.querySelector('input[type="search"]') as HTMLInputElement;
    const setter = Object.getOwnPropertyDescriptor(win.HTMLInputElement.prototype, "value")!.set!;
    setter.call(search, "Confirm API schema");
    search.dispatchEvent(new win.Event("input", { bubbles: true }));
    await new Promise((r) => setTimeout(r, 200));
  });
  await settle(() => {
    assert(host.querySelectorAll("[data-channel-id]").length === 1, "search filters to matching room");
    assert(text().includes("Confirm API schema contract draft"), "matching room title shown");
  });

  // 5. Read-only mode hides mutations + non-owner member can inspect Temp channels
  await act(async () => {
    root.render(React.createElement(Connections, { viewer: { operator: false, readOnly: true }, hostedBoard: true }));
  });
  await settle(() => {
    assert(document.getElementById("conn-tab-temp-channels") !== null, "member sees Temp channels tab");
    assert(!text().includes("Close channel") && !text().includes("Mark handled"), "readOnly hides mutating actions");
  });

  // 6. 404 degradation on older servers
  tempApiStatus = 404;
  await act(async () => {
    const refreshBtn = Array.from(document.querySelectorAll("button")).find((b) =>
      b.textContent?.includes("Refresh"),
    ) as HTMLButtonElement;
    refreshBtn.click();
  });
  await settle(() => {
    assert(text().includes("Temp channels audit unavailable"), "404 shows clean unavailable state");
  });

  // 7. Native board (hostedBoard: false) hides Temp channels tab and makes zero API calls
  const callsBeforeNative = tempApiCalls;
  await act(async () => {
    root.render(React.createElement(Connections, { viewer: { operator: true, readOnly: false }, hostedBoard: false }));
  });
  await settle(() => {
    assert(document.getElementById("conn-tab-temp-channels") === null, "native board omits Temp channels tab");
    assert(tempApiCalls === callsBeforeNative, "native board makes 0 temp-channels requests");
  });

  globalThis.fetch = innerFetch;
  await act(async () => root.unmount());
  console.log("tempChannelsUi: ok");
}

void main();
