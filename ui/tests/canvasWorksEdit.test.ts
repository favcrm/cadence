import type * as WorkspaceApps from "../src/features/workspace-apps/workspaceApps";
declare function require(name: string): any;
declare const process: any;
export {};

function assert(value: unknown, why: string): asserts value {
  if (!value) throw new Error(why);
}

const sampleRef = (over: Record<string, unknown> = {}) => ({
  installId: "install_works",
  tag: "works",
  contextId: "default",
  route: { section: "docs", view: "new" as const, record_id: null, segment_id: null },
  title: "New Doc",
  appLabel: "Works",
  href: "/apps/works?install=install_works&ctx=default&works=docs",
  ...over,
});

const WORKS_INSTALL = {
  install_id: "install_works",
  name: "works",
  title: "Works",
  version: "0.5.0",
  summary: "Docs, sheets, and decks.",
  digest: `sha256:${"a".repeat(64)}`,
  catalog_generation: "gen-1",
  storage_kind: "workspace",
  project_link: null,
  approved: true,
  executable: true,
  removed: null,
  approval: { state: "approved" },
  guide: "",
  files: [],
  capabilities: null,
  connection_slots: [],
  state: "active",
  navigation: {
    section_query: "works",
    default_section: "all",
    sections: [{ id: "all", label: "All" }, { id: "docs", label: "Docs" }],
  },
};

async function main() {
  const { Window } = require("happy-dom");
  const win = new Window({ url: "http://localhost/", settings: { disableIframePageLoading: true } });
  for (const name of [
    "window", "document", "location", "Node", "Element", "HTMLElement",
    "HTMLButtonElement", "HTMLIFrameElement", "SVGElement", "navigator",
    "MutationObserver", "ResizeObserver", "KeyboardEvent", "MouseEvent", "Event",
    "localStorage",
  ]) {
    if (win[name] !== undefined) {
      Object.defineProperty(globalThis, name, {
        value: name === "window" ? win : win[name],
        configurable: true,
        writable: true,
      });
    }
  }
  for (const name of ["addEventListener", "removeEventListener", "dispatchEvent", "requestAnimationFrame"]) {
    Object.defineProperty(globalThis, name, {
      value: win[name].bind(win),
      configurable: true,
      writable: true,
    });
  }
  Object.defineProperty(globalThis, "crypto", { value: require("crypto").webcrypto, configurable: true });
  Object.defineProperty(globalThis, "IS_REACT_ACT_ENVIRONMENT", { value: true });

  const loader = require("module");
  const originalRequire = loader.prototype.require;
  loader.prototype.require = function (this: unknown, id: string) {
    if (id.endsWith(".css")) return {};
    if (id === "@hugeicons/core-free-icons") return new Proxy({}, { get: () => ({}) });
    if (id === "@hugeicons/react") return { HugeiconsIcon: () => null };
    return originalRequire.apply(this, arguments);
  };

  let installFetches = 0;
  let resolveInitialInstallations!: (response: Response) => void;
  let deferInitialInstallations = false;
  const originalFetch = globalThis.fetch;
  globalThis.fetch = (async (input: unknown) => {
    const url = String(input);
    if (url.includes("/api/app-installations/install_works/contexts")) {
      return new Response(
        JSON.stringify({ contexts: [{ id: "default", install_id: "install_works", revision: 1, digest: WORKS_INSTALL.digest, state: "active", config: { schema: 1, label: "Default", input_defaults: {} } }] }),
        { status: 200, headers: { "Content-Type": "application/json" } },
      );
    }
    if (url.includes("/api/app-installations/install_works")) {
      return new Response(JSON.stringify(WORKS_INSTALL), {
        status: 200,
        headers: { "Content-Type": "application/json" },
      });
    }
    if (url.includes("/api/app-installations")) {
      installFetches += 1;
      if (deferInitialInstallations) {
        deferInitialInstallations = false;
        return new Promise<Response>((resolve) => { resolveInitialInstallations = resolve; });
      }
      return new Response(JSON.stringify([WORKS_INSTALL]), {
        status: 200,
        headers: { "Content-Type": "application/json" },
      });
    }
    return new Response("{}", { status: 404 });
  }) as typeof fetch;

  try {
    const React = require("react") as typeof import("react");
    const { createRoot } = require("react-dom/client") as typeof import("react-dom/client");
    const { renderToStaticMarkup } = require("react-dom/server") as typeof import("react-dom/server");
    const { HireBlock, WriteGate } = require("../src/features/auth/WriteGate") as typeof import("../src/features/auth/WriteGate");
    const { LocaleProvider } = require("../src/lib/locale") as typeof import("../src/lib/locale");
    const { appViewId, appViewRef } = require("../src/features/canvas/appView") as typeof import("../src/features/canvas/appView");
    const store = require("../src/features/canvas/canvasStore") as typeof import("../src/features/canvas/canvasStore");
    const ViewHeader = require("../src/features/canvas/ViewHeader").default;
    const AppViewBody = require("../src/features/canvas/AppViewBody").default;
    const WorksCanvasLauncher = require("../src/features/canvas/WorksCanvasLauncher").default;
    const { dropInstallationsSnapshot, notifyAppsChanged } = require("../src/features/workspace-apps/workspaceApps") as typeof WorkspaceApps;

    const settle = async () => {
      await React.act(async () => {
        for (let i = 0; i < 40; i++) await Promise.resolve();
      });
    };
    const click = async (el: Element | null) => {
      assert(el instanceof HTMLElement, "expected clickable element");
      await React.act(async () => {
        el.dispatchEvent(new MouseEvent("click", { bubbles: true }));
      });
      await settle();
    };

    // 1. CAD-2471 fail-closed invariant: appViewRef strips any untrusted `editable` field.
    const stripped = appViewRef(sampleRef({ editable: true }));
    assert(stripped !== null && stripped.editable === undefined, "appViewRef must strip untrusted editable field");

    // 2. openAppView always opens read-only; openAppViewEditable opts into editing; setAppViewEditable toggles cleanly.
    store.resetCanvas("conv_1", "k", "app");
    assert(store.openAppView(sampleRef({ editable: true })), "openAppView opens valid ref");
    const id = appViewId(stripped!);
    assert(store.canvasSnapshot().tabs[0]?.app?.editable === undefined, "openAppView stores read-only tab even if input carried editable");

    assert(store.openAppViewEditable(sampleRef()), "openAppViewEditable enables edit mode");
    assert(store.canvasSnapshot().tabs[0]?.app?.editable === true, "openAppViewEditable marks tab editable");

    store.setAppViewEditable(id, false);
    assert(store.canvasSnapshot().tabs[0]?.app?.editable === undefined, "setAppViewEditable(false) returns tab to read-only");

    store.setAppViewEditable(id, true);
    assert(store.canvasSnapshot().tabs[0]?.app?.editable === true, "setAppViewEditable(true) enables editing");

    // Re-opening from a chat card resets editable back to read-only.
    assert(store.openAppView(sampleRef()), "openAppView focuses existing tab");
    assert(store.canvasSnapshot().tabs[0]?.app?.editable === undefined, "openAppView resets tab back to read-only");

    // 3. ViewHeader exposes Edit here / Done editing and status badge accurately.
    const roHtml = renderToStaticMarkup(
      React.createElement(
        LocaleProvider,
        null,
        React.createElement(ViewHeader, {
          mark: null,
          back: null,
          title: "Budget",
          appName: "Works",
          options: "",
          note: "Read-only here — open in Works to edit",
          editing: false,
          edit: { active: false, label: "Edit here", onToggle: () => {} },
          open: { label: "Open in Works", href: "/apps/works", onOpen: () => {} },
          expand: { expanded: false, enabled: true, onToggle: () => {} },
          onClose: () => {},
        }),
      ),
    );
    assert(roHtml.includes("Edit here") && roHtml.includes("Read-only"), "read-only header shows Edit here toggle and Read-only status");

    const editHtml = renderToStaticMarkup(
      React.createElement(
        LocaleProvider,
        null,
        React.createElement(ViewHeader, {
          mark: null,
          back: null,
          title: "Budget",
          appName: "Works",
          options: "",
          note: "Read-only here — open in Works to edit",
          editing: true,
          edit: { active: true, label: "Done editing", onToggle: () => {} },
          open: { label: "Open in Works", href: "/apps/works", onOpen: () => {} },
          expand: { expanded: false, enabled: true, onToggle: () => {} },
          onClose: () => {},
        }),
      ),
    );
    assert(editHtml.includes("Done editing") && editHtml.includes("Editing") && !editHtml.includes("data-read-only"), "editing header shows Done editing and Editing status");

    // 4. Refused cases: blocked WriteGate or HireBlock never renders WorksCanvasLauncher or editable AppViewBody.
    const host = document.createElement("div");
    document.body.appendChild(host);
    const root = createRoot(host);

    const mountLauncher = async (writeGate: string | null, hireBlock: string | null) => {
      await React.act(async () => {
        root.render(
          React.createElement(
            LocaleProvider,
            null,
            React.createElement(
              WriteGate.Provider,
              { value: writeGate },
              React.createElement(
                HireBlock.Provider,
                { value: hireBlock },
                React.createElement(WorksCanvasLauncher, null),
              ),
            ),
          ),
        );
      });
      await settle();
    };

    dropInstallationsSnapshot();
    installFetches = 0;
    await mountLauncher("Member cannot write", null);
    assert(host.querySelector("[data-works-canvas-new]") === null, "WriteGate hides WorksCanvasLauncher");
    assert(installFetches === 0, "blocked launcher skips installation fetch");

    await mountLauncher(null, "Hiring blocked");
    assert(host.querySelector("[data-works-canvas-new]") === null, "HireBlock hides WorksCanvasLauncher");
    assert(installFetches === 0, "hire-blocked launcher skips installation fetch");
    // A mutation while the initial list is pending invalidates that read. The
    // launcher must request a fresh list rather than silently losing Works.
    deferInitialInstallations = true;
    await mountLauncher(null, null);
    assert(!deferInitialInstallations && host.querySelector("[data-works-canvas-new]") === null, "initial Works read is pending");
    await React.act(async () => { notifyAppsChanged(); });
    await settle();
    assert(Number(installFetches) === 2 && host.querySelector("[data-works-canvas-new]") !== null, "app mutation refreshes the launcher list");
    await React.act(async () => {
      resolveInitialInstallations(new Response(JSON.stringify([]), { status: 200, headers: { "Content-Type": "application/json" } }));
    });
    await settle();
    assert(host.querySelector("[data-works-canvas-new]") !== null, "invalidated initial answer cannot replace the fresh list");


    store.resetCanvas("conv_1", "k", "app");
    await mountLauncher(null, null);
    const newBtn = host.querySelector("[data-works-canvas-new]");
    assert(newBtn !== null && installFetches >= 1, "unblocked operator sees WorksCanvasLauncher");
    await click(newBtn);
    const menuItems = Array.from(host.querySelectorAll('[role="menuitem"]')).map((n) => n.textContent);
    assert(
      menuItems.includes("New Doc") &&
        menuItems.includes("New Sheet") &&
        menuItems.includes("New Deck") &&
        menuItems.includes("Import file…"),
      "launcher menu lists New Doc, New Sheet, New Deck, Import file…",
    );
    await click(Array.from(host.querySelectorAll('[role="menuitem"]')).find((n) => n.textContent === "Import file…") ?? null);
    assert(
      store.canvasSnapshot().tabs[0]?.app?.editable === true &&
        store.canvasSnapshot().tabs[0]?.app?.route.section === "all" &&
        store.canvasSnapshot().tabs[0]?.app?.route.view === "new",
      "clicking Import file… opens an editable Works Canvas tab",
    );

    store.resetCanvas("conv_1", "k", "app");
    await click(host.querySelector("[data-works-canvas-new]"));
    await click(host.querySelector('[role="menuitem"]'));
    assert(
      store.canvasSnapshot().tabs[0]?.app?.editable === true &&
        store.canvasSnapshot().tabs[0]?.app?.route.section === "docs" &&
        store.canvasSnapshot().tabs[0]?.app?.route.view === "new",
      "clicking New Doc opens an editable Works Canvas tab",
    );

    const editableTab = store.canvasSnapshot().tabs[0]!;
    const mountBody = async (writeGate: string | null, hireBlock: string | null) => {
      await React.act(async () => {
        root.render(
          React.createElement(
            LocaleProvider,
            null,
            React.createElement(
              WriteGate.Provider,
              { value: writeGate },
              React.createElement(
                HireBlock.Provider,
                { value: hireBlock },
                React.createElement(AppViewBody, {
                  tab: editableTab,
                  app: editableTab.app!,
                  sheet: false,
                  tabbed: false,
                  tabRow: undefined,
                  expanded: false,
                  canExpand: true,
                  onExpand: () => {},
                  onClose: () => {},
                }),
              ),
            ),
          ),
        );
      });
      await settle();
    };

    await mountBody("Member cannot write", null);
    assert(
      host.querySelector("[data-canvas-edit]") === null &&
        host.querySelector("[data-canvas-editing]") === null &&
        host.querySelector("[data-read-only]") !== null,
      "WriteGate forces AppViewBody read-only even when app.editable is true",
    );

    await mountBody(null, "Hire blocked");
    assert(
      host.querySelector("[data-canvas-edit]") === null &&
        host.querySelector("[data-canvas-editing]") === null &&
        host.querySelector("[data-read-only]") !== null,
      "HireBlock forces AppViewBody read-only even when app.editable is true",
    );

    await mountBody(null, null);
    assert(
      host.querySelector("[data-canvas-edit]")?.textContent === "Done editing" &&
        host.querySelector("[data-canvas-editing]")?.textContent === "Editing",
      "unblocked operator sees Done editing and Editing on editable tab",
    );

    await React.act(async () => {
      root.unmount();
    });
    host.remove();
    console.log("canvasWorksEdit: ok");
  } finally {
    globalThis.fetch = originalFetch;
    loader.prototype.require = originalRequire;
  }
}

main().catch((error) => {
  console.error(error);
  process.exit(1);
});
