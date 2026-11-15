export {};
declare function require(name: string): any;

function assert(value: unknown, why: string): asserts value {
  if (!value) throw new Error(why);
}

async function main() {
  const { Window } = require("happy-dom");
  const win = new Window({ url: "http://localhost/" });
  for (const name of ["window", "document", "Node", "Element", "HTMLElement", "HTMLInputElement", "Event", "MouseEvent", "KeyboardEvent"]) {
    Object.defineProperty(globalThis, name, {
      value: name === "window" ? win : win[name],
      configurable: true,
      writable: true,
    });
  }
  Object.defineProperty(globalThis, "IS_REACT_ACT_ENVIRONMENT", { value: true, configurable: true });

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
  const {
    FilterBar,
    StatusPill,
    StagedApprovalBanner,
    ConfirmDialog,
    LocaleBridgeProvider,
    useScreenLocale,
    useTranslate,
  } = require("../screen-ui/src/index") as typeof import("../screen-ui/src/index");

  let localeValue = "zh-TW";
  let useContextFallback = false;
  const listeners = new Set<() => void>();
  const fakeBridge = {
    getState: () =>
      useContextFallback
        ? { snapshot: null, context: { locale: localeValue } }
        : { snapshot: { presentationLocale: localeValue } },
    subscribe(fn: () => void) {
      listeners.add(fn);
      return () => listeners.delete(fn);
    },
  };

  const events: string[] = [];
  let query = "ada";
  let activeChip = true;
  let pending = false;

  function Probe() {
    const t = useTranslate();
    const screenLocale = useScreenLocale();
    return React.createElement(
      "div",
      null,
      React.createElement("span", { "data-testid": "locale-probe" }, `${screenLocale.locale}:${t("Save")}`),
      React.createElement(StatusPill, { tone: "ok", children: t("Save") }),
      React.createElement(StatusPill, { tone: "fail", children: "Blocked" }),
      React.createElement(FilterBar, {
        query,
        onQueryChange: (next) => {
          query = next;
          events.push(`query:${next}`);
        },
        chips: [{ id: "vip", label: "VIP", count: 4, active: activeChip }],
        onChipToggle: (id) => {
          activeChip = !activeChip;
          events.push(`chip:${id}`);
        },
        onClear: () => {
          query = "";
          activeChip = false;
          events.push("clear");
        },
      }),
      React.createElement(StagedApprovalBanner, {
        title: "Stage invoice",
        description: "Review before sending",
        tone: "warn",
        pending,
        onApprove: () => events.push("approve"),
        onDiscard: () => events.push("discard"),
      }),
      React.createElement(ConfirmDialog, {
        title: "Delete contact",
        body: "Permanent",
        expect: "12",
        match: "12",
        confirmLabel: "Delete",
        pending: false,
        error: null,
        onConfirm: () => events.push("confirm"),
        onCancel: () => events.push("cancel"),
      }),
    );
  }

  const host = win.document.createElement("div");
  win.document.body.append(host);
  const root = createRoot(host);

  try {
    const renderApp = async () => {
      await React.act(async () => {
        root.render(
          React.createElement(LocaleBridgeProvider, {
            bridge: fakeBridge,
            messages: {
              en: { Save: "Save", Clear: "Clear", Confirm: "Confirm", Discard: "Discard" },
              zh: { Save: "儲存", Clear: "清除", Confirm: "確認", Discard: "放棄" },
            },
            children: React.createElement(Probe),
          }),
        );
      });
    };

    await renderApp();

    assert(host.querySelector('[data-testid="locale-probe"]')?.textContent === "zh-TW:儲存", "zh-TW falls back to zh catalog");
    assert(host.querySelector('.s-status-pill[data-tone="ok"]')?.textContent === "儲存", "StatusPill ok renders translated children");
    assert(host.querySelector('.s-status-pill[data-tone="fail"]')?.textContent === "Blocked", "StatusPill fail renders tone");
    assert(host.querySelector(".s-confirm-wrap.crm-confirm-wrap"), "ConfirmDialog emits canonical .s-confirm-wrap alias");
    assert(host.querySelector(".s-confirm-scrim.crm-confirm-scrim"), "ConfirmDialog emits canonical .s-confirm-scrim alias");
    assert(host.querySelector(".s-confirm.crm-confirm"), "ConfirmDialog emits canonical .s-confirm alias");
    assert(host.querySelector(".s-field.crm-field"), "ConfirmDialog emits canonical .s-field alias");
    assert(host.querySelector(".s-toolbar.crm-toolbar"), "ConfirmDialog emits canonical .s-toolbar alias");

    const region = host.querySelector('.s-staged-banner[role="region"]');
    assert(region, "StagedApprovalBanner warn renders region");
    const buttons = Array.from(region.querySelectorAll("button")) as HTMLButtonElement[];
    assert(buttons.map((b) => b.textContent).join("|") === "放棄|確認", "StagedApprovalBanner uses LocaleBridgeProvider translations");
    buttons[0].click();
    buttons[1].click();
    assert(events.includes("discard") && events.includes("approve"), "StagedApprovalBanner fires callbacks");

    const search = host.querySelector('.s-filter-bar input[type="search"]') as HTMLInputElement;
    assert(search && search.value === "ada", "FilterBar binds search query");
    const chipBtn = host.querySelector('.s-filter-bar-chips button[aria-pressed="true"]') as HTMLButtonElement;
    assert(chipBtn && chipBtn.textContent === "VIP (4)", "FilterBar renders chip count and pressed state");
    chipBtn.click();
    const clearBtn = Array.from(host.querySelectorAll(".s-filter-bar-actions button"))[0] as HTMLButtonElement;
    assert(clearBtn && clearBtn.textContent === "清除", "FilterBar Clear button localized");
    clearBtn.click();
    assert(events.includes("chip:vip") && events.includes("clear"), "FilterBar chip and clear callbacks fire");

    await React.act(async () => {
      localeValue = "en";
      for (const fn of listeners) fn();
    });
    assert(host.querySelector('[data-testid="locale-probe"]')?.textContent === "en:Save", "LocaleBridgeProvider updates reactively on bridge push");

    await React.act(async () => {
      useContextFallback = true;
      localeValue = "zh-HK";
      for (const fn of listeners) fn();
    });
    assert(host.querySelector('[data-testid="locale-probe"]')?.textContent === "zh-HK:儲存", "LocaleBridgeProvider supports context.locale fallback");
    console.log("screen-ui 1.1 checks passed");
  } finally {
    await React.act(async () => {
      root.unmount();
    });
    win.close();
  }
}

void main();
