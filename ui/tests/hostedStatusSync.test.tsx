import type * as HappyDom from "happy-dom";
import type * as ReactNs from "react";
import type * as ReactDomClient from "react-dom/client";
import type * as LocaleModule from "../src/lib/locale";
import type HostedStatusType from "../src/features/settings/HostedStatus";

declare function require(name: string): unknown;
declare const process: { exit(code: number): never };
export {};

function assert(value: unknown, why: string): asserts value {
  if (!value) throw new Error(why);
}

function namedExport(mod: unknown, name: string): unknown {
  assert(mod && typeof mod === "object" && name in mod, `module must export ${name}`);
  return Reflect.get(mod, name);
}

async function main() {
  const { Window } = require("happy-dom") as typeof HappyDom;
  const win = new Window({ url: "http://localhost/settings/status" });
  for (const name of ["window", "document", "Node", "Element", "HTMLElement", "HTMLInputElement", "HTMLSelectElement", "HTMLTextAreaElement", "SVGElement", "navigator", "MutationObserver", "ResizeObserver", "Event", "MouseEvent", "KeyboardEvent", "location", "history", "sessionStorage", "localStorage"] as const) {
    Object.defineProperty(globalThis, name, { value: name === "window" ? win : win[name], configurable: true, writable: true });
  }
  for (const name of ["addEventListener", "removeEventListener", "requestAnimationFrame", "cancelAnimationFrame"] as const) {
    Object.defineProperty(globalThis, name, { value: win[name].bind(win), configurable: true, writable: true });
  }
  Object.defineProperty(globalThis, "IS_REACT_ACT_ENVIRONMENT", { value: true });

  const React = require("react") as typeof ReactNs;
  const { createRoot } = require("react-dom/client") as typeof ReactDomClient;
  const { LocaleProvider } = require("../src/lib/locale") as typeof LocaleModule;
  const HostedStatus = namedExport(require("../src/features/settings/HostedStatus"), "default") as typeof HostedStatusType;

  const host = document.createElement("div");
  document.body.appendChild(host);
  const root = createRoot(host);

  await React.act(async () => {
    root.render(React.createElement(LocaleProvider, null,
      React.createElement(HostedStatus, { meta: { build_commit: "dc74ea4a6fc0dee5" }, canReadConnections: false })));
  });
  assert(host.textContent?.includes("Board"), "Board row renders by default");
  assert(!host.textContent?.includes("Sync"), "Sync row stays hidden when partialUpdates is omitted/false");
  assert(!host.textContent?.includes("Some projections refresh periodically in the background"), "Sync description stays hidden when partialUpdates is false");

  await React.act(async () => {
    root.render(React.createElement(LocaleProvider, null,
      React.createElement(HostedStatus, { meta: { build_commit: "dc74ea4a6fc0dee5" }, canReadConnections: false, partialUpdates: true })));
  });
  assert(host.textContent?.includes("Sync"), "Sync row renders when partialUpdates is true");
  assert(host.textContent?.includes("Some projections refresh periodically in the background"), "Neutral sync note renders in English");
  assert(host.querySelector(".text-warn") === null, "Sync note uses neutral styling rather than text-warn");
  assert(!host.textContent?.includes("Some board updates are not live"), "Removed global warning string does not appear");

  await React.act(async () => { root.unmount(); });

  win.localStorage.setItem("cadence-locale", "zh-TW");
  const zhHost = document.createElement("div");
  document.body.appendChild(zhHost);
  const zhRoot = createRoot(zhHost);
  await React.act(async () => {
    zhRoot.render(React.createElement(LocaleProvider, null,
      React.createElement(HostedStatus, { meta: { build_commit: "dc74ea4a6fc0dee5" }, canReadConnections: false, partialUpdates: true })));
  });
  assert(zhHost.textContent?.includes("同步"), "Sync row translates to zh-TW");
  assert(zhHost.textContent?.includes("部分資料會在背景定期重新整理"), "Neutral sync note translates to zh-TW");
  assert(!zhHost.textContent?.includes("部分看板更新不是即時的"), "Removed global warning string does not appear in zh-TW");

  await React.act(async () => { zhRoot.unmount(); });
  console.log("hosted status sync checks passed");
}

void main().catch((e: unknown) => { console.error(e); process.exit(1); });
