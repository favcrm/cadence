declare function require(name: string): unknown;
export {};
function ok(value: unknown, why: string): asserts value {
  if (!value) throw new Error(why);
}

async function main() {
  const { Window } = require("happy-dom") as {
    Window: new (options: { url: string }) => Record<string, unknown>;
  };
  const win = new Window({ url: "http://localhost/" });
  for (const name of ["window", "document", "Node", "Element", "HTMLElement", "SVGElement", "navigator"])
    Object.defineProperty(globalThis, name, { value: name === "window" ? win : win[name], configurable: true, writable: true });
  Object.defineProperty(globalThis, "IS_REACT_ACT_ENVIRONMENT", { value: true });

  const calls: Array<{ id: string; body: { site_url: string } }> = [];
  const fakeApi = {
    protectedEntry: async () => ({
      configured: false,
      protected: false,
      origin: null,
      board_origin: "http://localhost",
    }),
    connectionSites: async () => ({
      sites: [
        { site_url: "sc-domain:example.test", permission_level: "siteOwner" },
        { site_url: "https://www.example.test/", permission_level: "siteFullUser" },
        { site_url: "https://legacy.example.test/", permission_level: "siteRestrictedUser" },
      ],
      selected: null,
    }),
    connectionSelectSite: async (id: string, body: { site_url: string }) => {
      calls.push({ id, body });
      return { site: { site_url: body.site_url, permission_level: "siteFullUser" } };
    },
  };

  const loader = require("module") as {
    prototype: { require: (id: string) => unknown };
  };
  const originalRequire = loader.prototype.require;
  loader.prototype.require = function (this: unknown, id: string) {
    if (id.endsWith(".css")) return {};
    if (id.endsWith("/lib/api")) return { api: fakeApi };
    if (id === "@hugeicons/core-free-icons") return new Proxy({}, { get: () => ({}) });
    if (id === "@hugeicons/react") return { HugeiconsIcon: () => null };
    return originalRequire.call(this, id);
  };

  const React = require("react") as typeof import("react");
  const { act } = require("react") as typeof import("react");
  const { createRoot } = require("react-dom/client") as typeof import("react-dom/client");
  const { ConnectionDetail } = require("../src/features/settings/Connections") as {
    ConnectionDetail: React.ComponentType<Record<string, unknown>>;
  };

  const mount = document.createElement("div");
  document.body.appendChild(mount);
  const root = createRoot(mount);
  const flush = async () => {
    await act(async () => {});
    await act(async () => {});
  };

  const row = {
    id: "con_synthetic",
    provider: "google_search_console",
    account: "owner@example.test",
    kind: "enrolled",
    revision: 3,
    registration_digest: null,
    scopes: ["webmasters.readonly"],
    read: { slug: "search_console_report", qualified: true, pinned: true, projected: true },
    status: {
      adapter_registered: true,
      descriptor_available: false,
      custody_available: true,
      manifest_status: "missing",
      reviewed_pin: null,
      reported_pin: null,
      execution_authority: true,
      network_checked: false,
    },
  };

  await act(async () => {
    root.render(
      React.createElement(ConnectionDetail, {
        row,
        provider: null,
        capabilities: null,
        canWrite: true,
        hostedBoard: true,
        onChanged: () => {},
        onRevoked: () => {},
      }),
    );
  });
  await flush();

  const text = () => mount.textContent ?? "";
  ok(text().includes("Search Console property"), "property section renders");
  ok(text().includes("sc-domain:example.test"), "exact property identifier renders");
  ok(text().includes("search_console_report"), "qualified read names its capability");
  const radios = Array.from(mount.querySelectorAll('input[type="radio"]')) as HTMLInputElement[];
  ok(radios.length === 3, `three property radios render: ${radios.length}`);
  ok(radios[2]!.disabled, "a restricted property cannot be selected");

  await act(async () => {
    radios[1]!.click();
  });
  const buttons = Array.from(mount.querySelectorAll("button")) as HTMLButtonElement[];
  const pin = buttons.find((button) => button.textContent?.includes("Pin property"));
  ok(pin, "Pin property action renders");
  ok(!pin!.disabled, "choosing a property enables Pin property");
  await act(async () => {
    pin!.click();
  });
  await flush();

  ok(calls.length === 1, `one selection request: ${calls.length}`);
  ok(calls[0]!.id === "con_synthetic", "selection targets the connection");
  ok(calls[0]!.body.site_url === "https://www.example.test/", "selection sends the exact property");
  ok(text().includes("Pinned. Search Console reports for this connection now use"), "success is explicit");
  ok(text().includes("Used by search_console_report"), "pinned line updates");

  await act(async () => {
    root.unmount();
  });
  console.log("Search Console property checks passed");
}

main().catch((e) => {
  setTimeout(() => {
    throw e;
  });
});
