export {};
/**
 * CAD-2764: ConnectCard self-updates from live `resources.connections` state
 * using the shared `useVisiblePoll` hook:
 *  - disconnected -> click Connect -> waiting -> Connected (on focus / poll)
 *  - expired/unhealthy row -> Reconnect needed + Reconnect button
 *  - member / read-only viewer makes zero `/api/connections` calls.
 */
declare function require(name: string): any;
declare const process: { exit(code?: number): never };

function assert(value: unknown, why: string): asserts value {
  if (!value) throw new Error(why);
}

async function main() {
  const { Window } = require("happy-dom");
  const win = new Window({ url: "http://localhost/home" });
  for (const name of [
    "window",
    "document",
    "Node",
    "Element",
    "HTMLElement",
    "SVGElement",
    "navigator",
    "MutationObserver",
    "ResizeObserver",
    "Event",
    "MouseEvent",
    "location",
    "sessionStorage",
    "localStorage",
  ]) {
    Object.defineProperty(globalThis, name, {
      value: name === "window" ? win : win[name],
      configurable: true,
      writable: true,
    });
  }
  for (const name of ["addEventListener", "removeEventListener", "dispatchEvent"]) {
    Object.defineProperty(globalThis, name, {
      value: win[name].bind(win),
      configurable: true,
      writable: true,
    });
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
  const { createRoot } = require("react-dom/client") as typeof import("react-dom/client");
  const { LocaleProvider } = require("../src/lib/locale") as typeof import("../src/lib/locale");
  const { WriteGate } = require("../src/features/auth/WriteGate") as typeof import("../src/features/auth/WriteGate");
  const { ConnectCard, toolkitConnectionState } = require("../src/features/home/ConnectCard") as typeof import("../src/features/home/ConnectCard");
  const typeMod = {} as { Connection: import("../src/lib/types").Connection };
  void typeMod;

  type Connection = import("../src/lib/types").Connection;
  type ThreadEntry = import("../src/features/home/thread").ThreadEntry;

  const makeRow = (over: Partial<Connection> = {}): Connection => ({
    id: "con_li_1",
    provider: "linkedin",
    account: "Chris Chiang",
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
      execution_authority: false,
      network_checked: false,
    },
    read: { slug: "linkedin_get_my_info", qualified: true, pinned: true, projected: true },
    hosted: {
      label: "LinkedIn",
      visibility: "personal",
      connected_by_you: true,
      connected_at: 1795000000000,
      last_used_at: null,
      can_change_visibility: true,
      tools: [{ slug: "linkedin_get_my_info", label: "LinkedIn: profile", effect: "read" }],
    },
    ...over,
  });

  // ---- Pure toolkitConnectionState derivation ----
  {
    assert(
      toolkitConnectionState("linkedin", "LinkedIn", null).status === "disconnected",
      "null rows -> disconnected",
    );
    assert(
      toolkitConnectionState("linkedin", "LinkedIn", []).status === "disconnected",
      "empty rows -> disconnected",
    );
    assert(
      toolkitConnectionState("linkedin", "LinkedIn", [makeRow({ kind: "builtin" })]).status === "disconnected",
      "builtin row ignored -> disconnected",
    );
    const active = toolkitConnectionState("linkedin", "LinkedIn", [makeRow()]);
    assert(active.status === "connected" && active.account === "Chris Chiang", "active row carries distinct account");
    const sameLabel = toolkitConnectionState("linkedin", "LinkedIn", [makeRow({ account: "LinkedIn" })]);
    assert(sameLabel.status === "connected" && sameLabel.account === null, "label-equal account omitted");
    const expired = toolkitConnectionState("linkedin", "LinkedIn", [
      makeRow({ status: { ...makeRow().status, custody_available: false } }),
    ]);
    assert(expired.status === "reconnect" && expired.account === null, "unhealthy row -> reconnect");
    const mixed = toolkitConnectionState("linkedin", "LinkedIn", [
      makeRow({ id: "old", status: { ...makeRow().status, custody_available: false } }),
      makeRow({ id: "new", account: "Latest Account" }),
    ]);
    assert(mixed.status === "connected" && mixed.account === "Latest Account", "active row wins over stale row");
  }

  // ---- Component reactive lifecycle ----
  let serverRows: Connection[] = [];
  let connectionFetches = 0;
  const connectStarts: string[] = [];
  const openedUrls: string[] = [];

  (win as { open: unknown }).open = (url: string) => {
    openedUrls.push(url);
    return null;
  };

  const json = (body: unknown, status = 200) =>
    new Response(JSON.stringify(body), { status, headers: { "Content-Type": "application/json" } });

  globalThis.fetch = (async (input: unknown, init?: RequestInit) => {
    const url = new URL(String(input), "http://localhost");
    if (url.pathname === "/api/connections") {
      connectionFetches += 1;
      return json({ connections: serverRows });
    }
    if (url.pathname === "/api/connection-providers/linkedin/connect" && init?.method === "POST") {
      connectStarts.push(url.pathname);
      return json({ ok: true, hosted: true, url: "https://connect.composio.dev/s/li_1" });
    }
    return json({}, 404);
  }) as typeof fetch;

  const entry: ThreadEntry = {
    seq: 1,
    role: "agent",
    kind: "connect_ref",
    message: "msg_1",
    text: "Connect LinkedIn — open Settings › Connections to finish.",
    payload: { v: 1, toolkit: "linkedin", toolkit_label: "LinkedIn" },
  };

  const flush = () =>
    React.act(async () => {
      await new Promise((resolve) => setTimeout(resolve, 0));
    });

  async function settle(check: () => void) {
    for (let i = 0; i < 60; i += 1) {
      await flush();
      try {
        check();
        return;
      } catch {
        await React.act(async () => {
          await new Promise((resolve) => setTimeout(resolve, 10));
        });
      }
    }
    check();
  }

  const mountCard = async (gate: string | null = null) => {
    const host = document.createElement("div");
    document.body.append(host);
    const root = createRoot(host);
    await React.act(async () => {
      root.render(
        React.createElement(
          LocaleProvider,
          null,
          React.createElement(
            WriteGate.Provider,
            { value: gate },
            React.createElement(ConnectCard, { entry }),
          ),
        ),
      );
    });
    return {
      host,
      card: () => host.querySelector('[data-kind="connect_ref"]') as HTMLElement | null,
      text: () => host.textContent?.replace(/\s+/g, " ").trim() ?? "",
      unmount: async () => {
        await React.act(async () => {
          root.unmount();
        });
        host.remove();
      },
    };
  };

  // 1. Member / non-owner viewer renders plain line and never fetches `/api/connections`.
  {
    connectionFetches = 0;
    const view = await mountCard("Member read-only");
    await flush();
    assert(connectionFetches === 0, "member view makes 0 /api/connections calls");
    assert(view.text().includes("Connect LinkedIn"), "member view keeps text line");
    assert(view.host.querySelector("button") === null, "member view renders no connect button");
    await view.unmount();
  }

  // 2. Owner: disconnected -> click Connect (waiting) -> server connects -> focus refresh -> Connected.
  {
    serverRows = [];
    connectionFetches = 0;
    const view = await mountCard(null);
    await settle(() => {
      assert(connectionFetches >= 1, "owner view revalidates /api/connections on mount");
      assert(
        view.card()?.getAttribute("data-connect-state") === "disconnected",
        "starts disconnected",
      );
      assert(view.host.querySelector('[data-connect-toolkit="linkedin"]') !== null, "shows Connect button");
    });

    await React.act(async () => {
      (view.host.querySelector('[data-connect-toolkit="linkedin"]') as HTMLButtonElement).click();
    });
    await settle(() => {
      assert(connectStarts.length === 1, "called /api/connection-providers/linkedin/connect");
      assert(openedUrls[0] === "https://connect.composio.dev/s/li_1", "opened hosted OAuth URL");
      assert(view.card()?.getAttribute("data-connect-state") === "waiting", "entered waiting state");
      assert(view.text().includes("Waiting…"), "renders Waiting status pill");
    });

    // OAuth finishes in the other tab and user returns (window focus event triggers refresh automatically).
    serverRows = [makeRow()];
    await React.act(async () => {
      win.dispatchEvent(new win.Event("focus"));
    });
    await settle(() => {
      assert(view.card()?.getAttribute("data-connect-state") === "connected", "auto-updated to connected");
      assert(view.text().includes("Connected"), "shows Connected pill");
      assert(view.text().includes("LinkedIn connected · Chris Chiang"), "shows connected account");
      assert(view.host.querySelector('[data-connect-toolkit="linkedin"]') === null, "hides Connect button once connected");
    });
    await view.unmount();
  }

  // 3. Expired / unhealthy row -> Reconnect needed state + Reconnect button.
  {
    serverRows = [
      makeRow({
        status: { ...makeRow().status, custody_available: false },
      }),
    ];
    const view = await mountCard(null);
    await settle(() => {
      assert(view.card()?.getAttribute("data-connect-state") === "reconnect", "expired row -> reconnect state");
      assert(view.text().includes("Reconnect needed"), "shows Reconnect needed pill");
      assert(
        view.host.querySelector('[data-connect-toolkit="linkedin"]')?.textContent?.includes("Reconnect LinkedIn"),
        "shows Reconnect LinkedIn button",
      );
    });
    await view.unmount();
  }
}

main().catch((e) => {
  console.error(e);
  process.exit(1);
});
