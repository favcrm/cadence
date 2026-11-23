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
  for (const name of ["window", "document", "Node", "Element", "HTMLElement", "SVGElement", "navigator"]) {
    Object.defineProperty(globalThis, name, {
      value: name === "window" ? win : win[name],
      configurable: true,
      writable: true,
    });
  }
  Object.defineProperty(globalThis, "IS_REACT_ACT_ENVIRONMENT", { value: true });

  const metaAssets = Array.from({ length: 10 }, (_, idx) => {
    const n = idx + 1;
    return {
      value: `act_100000000${n}`,
      label: n === 10 ? "Archive Account (Closed)" : `Synthetic Brand ${n}`,
      permission: n === 10 ? "disabled" : "active",
      selectable: n !== 10,
      currency: "HKD",
      timezone: "Asia/Hong_Kong",
    };
  });

  const googleAssets = [
    {
      value: "4128903310",
      label: "412-890-3310",
      permission: "accessible",
      selectable: true,
      currency: "USD",
      timezone: "America/Los_Angeles",
    },
    {
      value: "9988776655",
      label: "998-877-6655",
      permission: "read_only",
      selectable: false,
    },
  ];

  const calls: Array<{ id: string; body: { value: string } }> = [];
  const callCount = () => calls.length;
  let failOnce = false;

  const fakeApi = {
    protectedEntry: async () => ({
      configured: false,
      protected: false,
      origin: null,
      board_origin: "http://localhost",
    }),
    connectionAssets: async (id: string) => {
      if (failOnce) {
        failOnce = false;
        throw new Error("Temporary provider outage");
      }
      if (id === "con_google_syn") {
        return { kind: "google_ads_customer", assets: googleAssets, selected: null };
      }
      return { kind: "meta_ad_account", assets: metaAssets, selected: null };
    },
    connectionSelectAsset: async (id: string, body: { value: string }) => {
      calls.push({ id, body });
      if (id === "con_google_syn") {
        return {
          asset: {
            kind: "google_ads_customer",
            value: "4128903310",
            label: "412-890-3310",
            permission: "accessible",
          },
        };
      }
      const matched = metaAssets.find((entry) => entry.value === body.value)!;
      return {
        asset: {
          kind: "meta_ad_account",
          value: matched.value,
          label: matched.label,
          permission: matched.permission,
        },
      };
    },
  };

  const loader = require("module") as {
    prototype: { require: (id: string) => unknown };
  };
  const originalRequire = loader.prototype.require;
  loader.prototype.require = function (this: unknown, id: string) {
    if (id.endsWith(".css")) return {};
    if (id.endsWith("/lib/api")) return { api: fakeApi, ApiError: Error };
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

  const baseStatus = {
    adapter_registered: true,
    descriptor_available: false,
    custody_available: true,
    manifest_status: "matched",
    reviewed_pin: "hosted-connect/1",
    reported_pin: "hosted-connect/1",
    execution_authority: true,
    network_checked: false,
  };

  const metaRow = {
    id: "con_meta_syn",
    provider: "metaads",
    account: "Meta Ads",
    kind: "enrolled",
    revision: 1,
    registration_digest: null,
    scopes: [],
    read: { slug: "meta_ads_list_accounts", qualified: true, pinned: true, projected: true },
    status: baseStatus,
  };

  // 1. Meta Ads: filter input appears when >8 accounts, unselectable disabled, pin saves cleanly.
  await act(async () => {
    root.render(
      React.createElement(ConnectionDetail, {
        row: metaRow,
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
  ok(text().includes("Meta ad account"), "Meta ad account heading renders");
  ok(text().includes("Showing 10 of 10"), "account filter counter renders for >8 assets");

  const initialPinMeta = Array.from(mount.querySelectorAll("button")).find((b) =>
    b.textContent?.includes("Pin ad account"),
  ) as HTMLButtonElement | undefined;
  ok(initialPinMeta !== undefined && initialPinMeta.disabled, "unpinned Meta button reads Pin ad account (disabled)");
  ok(text().includes("Select an ad account above to pin."), "unpinned Meta guidance renders");
  ok(!text().includes("This ad account is already pinned."), "unpinned Meta does not claim already pinned");

  const radios = () => Array.from(mount.querySelectorAll('input[type="radio"]')) as HTMLInputElement[];
  ok(radios().length === 10, `10 Meta radios render: ${radios().length}`);
  ok(radios()[9]!.disabled, "unselectable Meta account radio is disabled");

  // Filter input narrows the visible radios.
  const searchInput = mount.querySelector('input[type="search"]') as HTMLInputElement | null;
  ok(searchInput !== null, "search filter input renders when > 8 assets");
  await act(async () => {
    const nativeInputValueSetter = Object.getOwnPropertyDescriptor(
      win.HTMLInputElement && (win.HTMLInputElement as { prototype: object }).prototype,
      "value",
    )?.set;
    if (nativeInputValueSetter) nativeInputValueSetter.call(searchInput, "Brand 2");
    else searchInput!.value = "Brand 2";
    searchInput!.dispatchEvent(new (win.Event as typeof Event)("input", { bubbles: true }));
  });
  await flush();
  ok(radios().length === 1, `filter narrows to 1 account: ${radios().length}`);
  ok(radios()[0]!.value === "act_1000000002", "filtered account is act_1000000002");

  await act(async () => {
    radios()[0]!.click();
  });
  const pinMeta = Array.from(mount.querySelectorAll("button")).find((b) =>
    b.textContent?.includes("Pin ad account"),
  ) as HTMLButtonElement | undefined;
  ok(pinMeta && !pinMeta.disabled, "Pin ad account button enabled after selecting");
  await act(async () => {
    pinMeta!.click();
  });
  await flush();
  ok(callCount() === 1 && calls[0]!.id === "con_meta_syn", "Meta pin targeted con_meta_syn");
  ok(calls[0]!.body.value === "act_1000000002", "Meta pin sent exact act_1000000002");
  ok(
    text().includes("Pinned. Meta Ads tools for this connection now use Synthetic Brand 2 (act_1000000002)."),
    "Meta pin confirmation displays label and value",
  );
  ok(text().includes("Used by Meta Ads tools"), "Meta status footer updates");
  const pinnedMetaBtn = Array.from(mount.querySelectorAll("button")).find(
    (b) => b.textContent?.trim() === "Pinned",
  ) as HTMLButtonElement | undefined;
  ok(pinnedMetaBtn !== undefined && pinnedMetaBtn.disabled, "post-pin Meta button reads Pinned (disabled)");
  ok(text().includes("This ad account is already pinned."), "post-pin Meta shows already pinned hint");

  // 2. Google Ads: renders customer IDs and handles normalized server value response.
  const googleRow = {
    ...metaRow,
    id: "con_google_syn",
    provider: "googleads",
    account: "Google Ads",
    read: { slug: "google_ads_list_accounts", qualified: true, pinned: true, projected: true },
  };
  await act(async () => {
    root.render(
      React.createElement(ConnectionDetail, {
        row: googleRow,
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
  ok(text().includes("Google Ads customer"), "Google Ads customer heading renders");
  ok(text().includes("412-890-3310"), "Google Ads formatted customer label renders");
  const initialPinGoogle = Array.from(mount.querySelectorAll("button")).find((b) =>
    b.textContent?.includes("Pin customer"),
  ) as HTMLButtonElement | undefined;
  ok(initialPinGoogle !== undefined && initialPinGoogle.disabled, "unpinned Google button reads Pin customer (disabled)");
  ok(text().includes("Select a customer ID above to pin."), "unpinned Google guidance renders");
  ok(!text().includes("This customer ID is already pinned."), "unpinned Google does not claim already pinned");
  ok(radios().length === 2 && radios()[1]!.disabled, "unselectable Google Ads customer disabled");
  await act(async () => {
    radios()[0]!.click();
  });
  const pinGoogle = Array.from(mount.querySelectorAll("button")).find((b) =>
    b.textContent?.includes("Pin customer"),
  ) as HTMLButtonElement | undefined;
  ok(pinGoogle && !pinGoogle.disabled, "Pin customer button enabled");
  await act(async () => {
    pinGoogle!.click();
  });
  await flush();
  ok(callCount() === 2 && calls[1]!.id === "con_google_syn", "Google pin targeted con_google_syn");
  ok(
    text().includes("Pinned. Google Ads tools for this connection now use 412-890-3310 (4128903310)."),
    "Google pin confirmation displays normalized value",
  );
  const pinnedGoogleBtn = Array.from(mount.querySelectorAll("button")).find(
    (b) => b.textContent?.trim() === "Pinned",
  ) as HTMLButtonElement | undefined;
  ok(pinnedGoogleBtn !== undefined && pinnedGoogleBtn.disabled, "post-pin Google button reads Pinned (disabled)");
  ok(text().includes("This customer ID is already pinned."), "post-pin Google shows already pinned hint");

  // 3. Non-owner view (`canWrite: false`): radios disabled and no Pin button.
  await act(async () => {
    root.render(
      React.createElement(ConnectionDetail, {
        row: metaRow,
        provider: null,
        capabilities: null,
        canWrite: false,
        hostedBoard: true,
        onChanged: () => {},
        onRevoked: () => {},
      }),
    );
  });
  await flush();
  ok(text().includes("Only the workspace owner can pin an account."), "non-owner guidance renders");
  ok(radios().every((r) => r.disabled), "all radios disabled when canWrite is false");
  ok(
    !Array.from(mount.querySelectorAll("button")).some((b) =>
      b.textContent?.includes("Pin ad account"),
    ),
    "no Pin button shown for non-owner",
  );

  // 4. Error state and Try again reload recovery.
  failOnce = true;
  await act(async () => {
    root.render(
      React.createElement(ConnectionDetail, {
        row: googleRow,
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
  const tryAgain = Array.from(mount.querySelectorAll("button")).find((b) =>
    b.textContent?.includes("Try again"),
  ) as HTMLButtonElement | undefined;
  ok(tryAgain !== undefined, "error recovery shows Try again button");
  await act(async () => {
    tryAgain!.click();
  });
  await flush();
  ok(radios().length === 2, "Try again reloads asset list");

  await act(async () => {
    root.unmount();
  });
  console.log("Connection asset picker checks passed");
}

main().catch((e) => {
  setTimeout(() => {
    throw e;
  });
});
