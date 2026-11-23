export {};
/**
 * CAD-2726: AccountMenu workspace switcher lazy-fetch, session cache & Billing shortcut.
 */
declare function require(name: string): any;

const { Window } = require("happy-dom");
const win = new Window({ url: "https://acme.cadencecloud.app/" });
for (const key of [
  "window", "document", "navigator", "HTMLElement", "Node", "Event",
  "KeyboardEvent", "location", "localStorage", "sessionStorage",
]) {
  Object.defineProperty(globalThis, key, {
    value: key === "window" ? win : win[key],
    configurable: true,
    writable: true,
  });
}
Object.defineProperty(globalThis, "IS_REACT_ACT_ENVIRONMENT", { value: true });

const loader = require("module");
const originalRequire = loader.prototype.require;
loader.prototype.require = function (this: unknown, id: string) {
  if (id === "@hugeicons/core-free-icons") return new Proxy({}, { get: () => ({}) });
  if (id === "@hugeicons/react") return { HugeiconsIcon: () => null };
  return originalRequire.apply(this, arguments);
};

const React = require("react") as typeof import("react");
const { createRoot } = require("react-dom/client") as typeof import("react-dom/client");
const { LocaleProvider } = require("../src/lib/locale") as typeof import("../src/lib/locale");
const { NavigationProvider } = require("../screen-ui/src/navigation") as typeof import("../screen-ui/src/navigation");
const AccountMenu = (require("../src/ui/AccountMenu") as typeof import("../src/ui/AccountMenu")).default;
type Meta = import("../src/lib/types").Meta;

function assert(value: unknown, why: string): asserts value {
  if (!value) throw new Error(`accountMenuWorkspaces test failed: ${why}`);
}

function equal(actual: unknown, expected: unknown, why: string): void {
  if (actual !== expected) throw new Error(`${why}: expected ${String(expected)}, got ${String(actual)}`);
}

async function flush(): Promise<void> {
  await React.act(async () => {
    await new Promise((resolve) => setTimeout(resolve, 0));
  });
}

const adapter = {
  href: (path: string) => path,
  navigate: () => undefined,
  modifiedClicks: "browser" as const,
};

const baseHosted = {
  hosted: true,
  read_only: false,
  signed_in: true,
  actor: "Fable",
  platform_account_configured: true,
  session: {
    id: "sess-ws-1",
    origin: "public",
    user: { name: "Fable", email: "fable@example.test", role: "owner", handle: "fable", sub: "u_1" },
  },
} as unknown as Meta;

async function main(): Promise<void> {
  const fetches: string[] = [];
  let wsStatus = 200;
  let wsBody: unknown = {
    currentWorkspaceId: "ws_1",
    createWorkspaceUrl: "https://app-v2.agenticos.hk/register?mode=new",
    workspaces: [
      {
        workspaceId: "ws_1",
        slug: "acme",
        name: "Acme Lab",
        nameZh: "Acme 實驗室",
        role: "owner",
        boardHost: "acme.cadencecloud.app",
        boardUrl: "https://acme.cadencecloud.app",
        boardState: "active",
        current: true,
      },
      {
        workspaceId: "ws_2",
        slug: "beta",
        name: "Beta Studio",
        nameZh: null,
        role: "member",
        boardHost: "beta.cadencecloud.app",
        boardUrl: "https://beta.cadencecloud.app",
        boardState: "active",
        current: false,
      },
      {
        workspaceId: "ws_prep",
        slug: "prep",
        name: "Prep Co",
        nameZh: null,
        role: "owner",
        boardHost: "prep.cadencecloud.app",
        boardUrl: "https://prep.cadencecloud.app",
        boardState: "prepared",
        current: false,
      },
      {
        workspaceId: "ws_bad",
        slug: "evil",
        name: "Unsafe Link Co",
        nameZh: null,
        role: "member",
        boardHost: "evil.test",
        boardUrl: "javascript:alert(1)",
        boardState: "active",
        current: false,
      },
    ],
  };

  globalThis.fetch = (async (input: RequestInfo | URL) => {
    const path = String(input);
    fetches.push(path);
    return new Response(JSON.stringify(wsBody), {
      status: wsStatus,
      headers: { "Content-Type": "application/json" },
    });
  }) as typeof fetch;

  const mount = (meta: Meta, billingHref?: string) => {
    const host = document.createElement("div");
    document.body.append(host);
    const root = createRoot(host);
    React.act(() => {
      root.render(
        React.createElement(
          NavigationProvider,
          {
            adapter,
            children: React.createElement(
              LocaleProvider,
              {
                children: React.createElement(AccountMenu, {
                  meta,
                  onChange: () => undefined,
                  trigger: "row",
                  placement: "above-start",
                  settingsHref: "/settings/preferences",
                  billingHref,
                }),
              },
            ),
          },
        ),
      );
    });
    return {
      host,
      trigger: () => host.querySelector("button[aria-haspopup=dialog]") as HTMLButtonElement,
      panel: () => host.querySelector("[role=dialog]"),
      done: () => {
        React.act(() => root.unmount());
        host.remove();
      },
    };
  };

  // 1. Non-hosted board never fetches workspaces even when opened
  {
    fetches.length = 0;
    const { trigger, panel, done } = mount({ ...baseHosted, hosted: false } as unknown as Meta);
    await React.act(async () => trigger().click());
    await flush();
    equal(fetches.filter((p) => p === "/api/account/workspaces").length, 0, "native board makes no workspaces read");
    assert(!panel()?.querySelector("[data-workspace-switcher]"), "switcher omitted on native board");
    done();
  }

  // 2. Hosted signed-in: zero reads while closed, lazy fetch once on open, cached on reopen
  {
    fetches.length = 0;
    const { trigger, panel, done } = mount(baseHosted, "/settings/account");
    await flush();
    equal(fetches.filter((p) => p === "/api/account/workspaces").length, 0, "zero fetches while menu is closed");

    await React.act(async () => trigger().click());
    await flush();
    equal(fetches.filter((p) => p === "/api/account/workspaces").length, 1, "fetches workspaces lazily when opened");

    const switcher = panel()?.querySelector("[data-workspace-switcher]");
    assert(switcher, "workspace switcher renders when hosted response succeeds");
    assert(switcher.textContent?.includes("Acme Lab") && switcher.textContent.includes("Current workspace"), "active workspace marked Current workspace");
    const betaLink = switcher.querySelector('a[href="https://beta.cadencecloud.app/"]') as HTMLAnchorElement | null;
    assert(betaLink && betaLink.textContent?.includes("Beta Studio"), "safe peer workspace renders link");
    assert(betaLink.textContent?.includes("Workspace member"), "peer role shows Workspace member");
    assert(switcher.textContent.includes("Prep Co") && !switcher.querySelector('a[href="https://prep.cadencecloud.app/"]'), "non-active workspace is non-clickable");
    assert(!switcher.querySelector('a[href^="javascript:"]'), "unsafe javascript workspace URL never rendered as anchor");
    const createLink = switcher.querySelector('a[href="https://app-v2.agenticos.hk/register?mode=new"]');
    assert(createLink && createLink.textContent?.includes("Create another workspace"), "create workspace link rendered");

    const billingLink = panel()?.querySelector('a[href="/settings/account"]');
    assert(billingLink && billingLink.textContent === "Billing", "Billing shortcut rendered in AccountMenu");

    // Close and reopen -> cache hit (still 1 fetch)
    await React.act(async () => trigger().click());
    await React.act(async () => trigger().click());
    await flush();
    equal(fetches.filter((p) => p === "/api/account/workspaces").length, 1, "cached across open/close");
    done();
  }

  // 2b. zh-TW locale uses non-colliding translations
  {
    win.localStorage.setItem("cadence-locale", "zh-TW");
    const { trigger, panel, done } = mount(
      { ...baseHosted, session: { ...baseHosted.session, id: "sess-ws-zh" } } as unknown as Meta,
      "/settings/account",
    );
    await React.act(async () => trigger().click());
    await flush();
    const switcher = panel()?.querySelector("[data-workspace-switcher]");
    assert(switcher, "zh-TW workspace switcher renders");
    assert(switcher.textContent?.includes("目前工作區"), "zh-TW Current workspace label shown");
    assert(switcher.textContent?.includes("工作區成員"), "zh-TW Workspace member label shown");
    assert(!switcher.textContent?.includes("進行中"), "zh-TW avoids collided Current='進行中'");
    done();
    win.localStorage.removeItem("cadence-locale");
  }

  // 3. 404 omits workspace switcher cleanly without errors
  {
    fetches.length = 0;
    wsStatus = 404;
    wsBody = { error: { code: "not_found" } };
    const { trigger, panel, done } = mount({ ...baseHosted, session: { ...baseHosted.session, id: "sess-ws-404" } } as unknown as Meta);
    await React.act(async () => trigger().click());
    await flush();
    assert(panel() && !panel()?.querySelector("[data-workspace-switcher]"), "404 hides switcher without crashing menu");
    done();
  }

  console.log("accountMenuWorkspaces checks passed");
}

declare const process: { exit: (code?: number) => void };
main().then(
  () => process.exit(0),
  (e) => {
    console.error(e);
    process.exit(1);
  },
);
