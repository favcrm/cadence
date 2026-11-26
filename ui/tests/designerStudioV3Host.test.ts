/**
 * CAD-2852: Designer Studio v3 host checks:
 * 1. InstalledScreenOutlet initial projection carries ACTION_VERBS when admitted & writable,
 *    and [] when readOnly or unadmitted (prevents cold-load media_read_not_advertised flash).
 * 2. planRunStart forwards declared inputs.model to bindingQuote as `?model=` when present.
 * 3. AppShell independent drawer displays "New design" for surface="create", sets data-closing
 *    when CSS transitionDuration > 0, and completes close cleanly on transitionend.
 */
import type * as ReactModule from "react";
import type * as ReactDomModule from "react-dom/client";
import type * as LocaleModule from "../src/lib/locale";
import type AppShellComponent from "../src/features/app-shell/AppShell";
import type { InstalledScreenOutlet as InstalledScreenOutletType } from "../src/features/workspace-apps/screen/ScreenHost";
import type { ActionContext } from "../src/features/workspace-apps/screen/screenActions";
import type { ACTION_VERBS } from "../src/features/workspace-apps/screen/screenProtocol";
import type { Installation } from "../src/features/workspace-apps/workspaceApps";
import type { Viewer } from "../src/features/projects/work";
declare function require(name: string): any;
declare const process: { cwd(): string };
export {};

function assert(value: unknown, why: string): asserts value {
  if (!value) throw new Error(why);
}

const { Window } = require("happy-dom");
const win = new Window({
  url: "http://localhost/app-installations/install-ds",
  settings: {
    fetch: {
      interceptor: {
        beforeAsyncRequest: async ({ window }: { window: any }) =>
          new window.Response("<!doctype html><title>Designer Studio</title>", {
            headers: { "Content-Type": "text/html" },
          }),
      },
    },
  },
});
for (const name of [
  "window", "document", "Node", "Element", "HTMLElement", "HTMLInputElement",
  "HTMLTextAreaElement", "HTMLDialogElement", "SVGElement", "navigator",
  "MutationObserver", "ResizeObserver", "Event", "CustomEvent", "MouseEvent",
  "KeyboardEvent", "FocusEvent", "TransitionEvent", "location", "history",
  "sessionStorage", "localStorage",
]) {
  Object.defineProperty(globalThis, name, {
    value: name === "window" ? win : win[name],
    configurable: true,
    writable: true,
  });
}
for (const name of ["getComputedStyle", "requestAnimationFrame", "cancelAnimationFrame"]) {
  Object.defineProperty(globalThis, name, { value: win[name].bind(win), configurable: true, writable: true });
}
for (const name of ["addEventListener", "removeEventListener", "dispatchEvent"]) {
  Object.defineProperty(globalThis, name, { value: win[name].bind(win), configurable: true, writable: true });
}
Object.defineProperty(globalThis, "crypto", { value: require("crypto").webcrypto, configurable: true });
Object.defineProperty(globalThis, "IS_REACT_ACT_ENVIRONMENT", { value: true });
let reducedMotion = false;
const matchMediaImpl = (query: string) => ({
  get matches() {
    return typeof query === "string" && query.includes("prefers-reduced-motion") ? reducedMotion : false;
  },
  media: query,
  onchange: null,
  addEventListener() {},
  removeEventListener() {},
  addListener() {},
  removeListener() {},
  dispatchEvent() { return false; },
});
Object.defineProperty(win, "matchMedia", { value: matchMediaImpl, configurable: true, writable: true });
Object.defineProperty(globalThis, "matchMedia", { value: matchMediaImpl, configurable: true, writable: true });

const loader = require("module");
const originalRequire = loader.prototype.require;
loader.prototype.require = function (this: unknown, id: string) {
  if (id.endsWith(".css")) return {};
  if (id === "@hugeicons/core-free-icons") return new Proxy({}, { get: () => ({}) });
  if (id === "@hugeicons/react") return { HugeiconsIcon: () => null };
  return originalRequire.apply(this, arguments);
};

const React = require("react") as typeof ReactModule;
const { createRoot } = require("react-dom/client") as typeof ReactDomModule;
const { LocaleProvider } = require("../src/lib/locale") as typeof LocaleModule;
const AppShell = require("../src/features/app-shell/AppShell").default as typeof AppShellComponent;
const { InstalledScreenOutlet } = require("../src/features/workspace-apps/screen/ScreenHost") as {
  InstalledScreenOutlet: typeof InstalledScreenOutletType;
};
const { makePlanner } = require("../src/features/workspace-apps/screen/screenActions") as {
  makePlanner: typeof import("../src/features/workspace-apps/screen/screenActions").makePlanner;
};
const { ACTION_VERBS: expectedVerbs } = require("../src/features/workspace-apps/screen/screenProtocol") as {
  ACTION_VERBS: typeof ACTION_VERBS;
};
const { readFileSync } = require("fs");

const flush = () => React.act(async () => { await Promise.resolve(); });

class Port {
  onmessage: ((event: { data: unknown }) => void) | null = null;
  onmessageerror: (() => void) | null = null;
  sent: Record<string, unknown>[] = [];
  closed = false;
  start() {}
  close() { this.closed = true; }
  postMessage(value: Record<string, unknown>) { this.sent.push(value); }
  receive(data: unknown) { this.onmessage?.({ data }); }
}

async function checkInitialActionsAndModelQuote() {
  const install = "install-ds";
  const digest = "sha256:" + "a".repeat(64);
  const receipt = {
    mount: `/api/app-screen/${"1".repeat(64)}`,
    bridge_nonce: "2".repeat(64),
    generation: 1,
    tag: "studio",
    action_token: "3".repeat(64),
    tools: [],
    media: { contract: "screen-media.v1", may: ["media.read", "media.publish", "media.chat.attach"] },
  };
  const installation: Installation = {
    install_id: install,
    name: "designer-studio",
    title: "Designer Studio",
    version: "0.1.8",
    summary: "",
    digest,
    catalog_generation: "1",
    storage_kind: "workspace",
    project_link: null,
    approved: true,
    executable: true,
    approval: { state: "approved" },
    guide: "",
    files: ["screens/studio/screens.json"],
    capabilities: { image: { schema: 1, capability: "media.generate", version: 1, action: "generate_image", resource_kind: "connection_account", effect: "draft" } },
    connection_slots: [],
    workflows: [
      {
        name: "create-image",
        label: "New image from brief",
        capability_slots: ["image"],
        execution: "host",
        inputs: [
          { name: "subject", ask: "Subject" },
          { name: "source", ask: "Source" },
          { name: "image_prompt", ask: "Prompt" },
          { name: "model", ask: "Model", optional: true },
          { name: "aspect_ratio", ask: "Aspect", optional: true },
        ],
      },
    ],
  };
  const contexts = [{ id: "default", install_id: install, revision: 1, digest, state: "active", config: { schema: 1, label: "Default", input_defaults: {} } }];

  const quoteUrls: string[] = [];
  globalThis.fetch = (async (input: RequestInfo | URL) => {
    const url = typeof input === "string" ? input : input instanceof URL ? input.href : input.url;
    if (url.includes("/mount")) {
      return new Response(JSON.stringify(receipt), { headers: { "Content-Type": "application/json" } });
    }
    if (url.includes("/quote")) {
      quoteUrls.push(url);
      const isMini = url.includes("model=openai%2Fgpt-image-2.5-mini");
      return new Response(JSON.stringify({
        slot: "image",
        model: isMini ? "openai/gpt-image-2.5-mini" : "openai/gpt-image-2.5",
        quote: {
          schema: 1,
          currency: "USD",
          unit_price_micros: isMini ? 20_000 : 90_000,
          units: 1,
          total_price_micros: isMini ? 20_000 : 90_000,
          price_revision: "v1",
        },
      }), { headers: { "Content-Type": "application/json" } });
    }
    return new Response("{}", { headers: { "Content-Type": "application/json" } });
  }) as typeof fetch;

  // 1a. Admitted writable InstalledScreenOutlet advertises media.read immediately on cold load (before snapshot settles).
  const host = document.createElement("section");
  document.body.append(host);
  const root = createRoot(host);
  await React.act(async () => {
    root.render(React.createElement(LocaleProvider, null, React.createElement(InstalledScreenOutlet, {
      installation,
      contexts: contexts as any,
      contextId: "default",
      admitted: true,
      fallback: React.createElement("div", null, "fallback"),
    })));
  });
  await flush();
  const frame = host.querySelector("iframe");
  assert(frame?.contentWindow, "cold-load screen mounted");
  const port = new Port();
  const event = new win.Event("message");
  for (const [k, v] of Object.entries({ source: frame.contentWindow, origin: "null", ports: [port], data: { v: 1, op: "init", tag: "studio", bridge_nonce: receipt.bridge_nonce, generation: 1 } })) {
    Object.defineProperty(event, k, { value: v });
  }
  await React.act(async () => {
    win.dispatchEvent(event);
    port.receive({ v: 1, op: "ready", accepts: ["screen.v2", "screen-media.v1", "screen-surfaces.v1"] });
  });
  await flush();
  const firstPush = port.sent.find((m) => m.op === "screen") as { actions?: string[] } | undefined;
  assert(firstPush && Array.isArray(firstPush.actions), "first push includes actions array");
  assert(firstPush.actions.includes("media.read"), "first push advertises media.read on cold load without waiting for snapshot");
  for (const v of expectedVerbs) {
    assert(firstPush.actions.includes(v), `cold-load push carries ${v}`);
  }
  await React.act(async () => root.unmount());
  host.remove();

  // 1b. Read-only InstalledScreenOutlet still advertises empty actions on cold load.
  const roHost = document.createElement("section");
  document.body.append(roHost);
  const roRoot = createRoot(roHost);
  await React.act(async () => {
    roRoot.render(React.createElement(LocaleProvider, null, React.createElement(InstalledScreenOutlet, {
      installation,
      contexts: contexts as any,
      contextId: "default",
      admitted: true,
      readOnly: true,
      fallback: React.createElement("div", null, "fallback"),
    })));
  });
  await flush();
  const roFrame = roHost.querySelector("iframe");
  assert(roFrame?.contentWindow, "read-only screen mounted");
  const roPort = new Port();
  const roEvent = new win.Event("message");
  for (const [k, v] of Object.entries({ source: roFrame.contentWindow, origin: "null", ports: [roPort], data: { v: 1, op: "init", tag: "studio", bridge_nonce: receipt.bridge_nonce, generation: 1 } })) {
    Object.defineProperty(roEvent, k, { value: v });
  }
  await React.act(async () => {
    win.dispatchEvent(roEvent);
    roPort.receive({ v: 1, op: "ready", accepts: ["screen.v2", "screen-media.v1", "screen-surfaces.v1"] });
  });
  await flush();
  const roPush = roPort.sent.find((m) => m.op === "screen") as { actions?: string[] } | undefined;
  assert(roPush && Array.isArray(roPush.actions) && roPush.actions.length === 0, "read-only push keeps actions empty");
  await React.act(async () => roRoot.unmount());
  roHost.remove();

  // 2. planRunStart forwards declared inputs.model as ?model= on bindingQuote.
  const planner = makePlanner(() => ({ installation, installId: install, contextId: "default", runs: [], onChanged: () => {} }) as ActionContext);
  const miniPlan = await planner("run.start", {
    workflow: "create-image",
    request_id: "req-mini-1",
    inputs: { subject: "S", source: "F", image_prompt: "P", model: "openai/gpt-image-2.5-mini", aspect_ratio: "16:9" },
  });
  assert("label" in miniPlan && miniPlan.label.includes("$0.02") && miniPlan.label.includes("openai/gpt-image-2.5-mini"), `mini quote label: ${JSON.stringify(miniPlan)}`);
  assert(quoteUrls.some((u) => u.endsWith("/bindings/image/quote?model=openai%2Fgpt-image-2.5-mini")), "bindingQuote carried ?model= param");

  const defaultPlan = await planner("run.start", {
    workflow: "create-image",
    request_id: "req-def-1",
    inputs: { subject: "S", source: "F", image_prompt: "P" },
  });
  assert("label" in defaultPlan && defaultPlan.label.includes("$0.09"), "default quote without model uses $0.09");
  assert(quoteUrls.some((u) => u.endsWith("/bindings/image/quote")), "bindingQuote omits ?model= when absent");
}

async function checkAppShellDrawerTitleAndMotion() {
  const css = readFileSync(`${process.cwd()}/src/features/app-shell/app-shell.css`, "utf8") as string;
  assert(/app-shell-drawer-in/.test(css) && /app-shell-screen-drawer\[data-closing="true"\]/.test(css), "app-shell.css defines enter and closing drawer motion");

  const install = "install-ds-shell";
  const installation = {
    install_id: install,
    name: "social-content",
    title: "Social Content",
    version: "0.5.4",
    summary: "",
    digest: "sha256:" + "b".repeat(64),
    catalog_generation: "1",
    storage_kind: "workspace" as const,
    project_link: null,
    approved: true,
    executable: true,
    approval: { state: "approved" },
    guide: "",
    files: ["screens/studio/screens.json"],
    capabilities: null,
    connection_slots: [],
  };
  const contexts = [{ id: "default", install_id: install, revision: 1, digest: "sha256:" + "c".repeat(64), state: "active", config: { schema: 1, label: "Default", input_defaults: {} } }];
  globalThis.fetch = (async (input: RequestInfo | URL) => {
    const path = typeof input === "string" ? input : input instanceof URL ? input.href : input.url;
    const json = (value: unknown) => new Response(JSON.stringify(value), { headers: { "Content-Type": "application/json" } });
    if (path === `/api/app-installations/${install}`) return json(installation);
    if (path === `/api/app-installations/${install}/contexts`) return json({ contexts });
    return json({});
  }) as typeof fetch;

  type ScreenBridge = { onScreenDrawer?: (mode: "outlet" | "drawer", surface?: string, independent?: boolean, size?: "narrow" | "wide") => void };
  let bridge: ScreenBridge | null = null;
  const host = document.createElement("section");
  document.body.append(host);
  const viewer: Viewer = { operator: true, readOnly: false };
  const root = createRoot(host);
  await React.act(async () => {
    root.render(React.createElement(LocaleProvider, null, React.createElement(AppShell, {
      installId: install,
      viewer,
      children: (_a: unknown, _c: string, screen: ScreenBridge) => {
        bridge = screen;
        return React.createElement("div", { "data-surface": "list" });
      },
    })));
  });
  for (let i = 0; i < 40 && !host.querySelector("[data-surface=list]"); i += 1) await flush();
  const drive = (bridge as ScreenBridge | null)?.onScreenDrawer;
  assert(drive, "AppShell hands independent drawer callback");

  await React.act(async () => { drive("drawer", "create", true, "narrow"); });
  await flush();
  const panel = host.querySelector(".app-shell-screen-drawer") as HTMLElement | null;
  assert(panel && panel.querySelector(".app-shell-screen-drawer-bar span")?.textContent === "New design", "surface=create shows New design bar title");

  // Simulate loaded CSS transitionDuration so closing state enters `data-closing="true"` and cleans up on transitionend.
  panel.style.transitionDuration = "0.2s";
  const closeBtn = panel.querySelector("button[aria-label='Close']") as HTMLElement;
  await React.act(async () => { closeBtn.click(); });
  await flush();
  const closingPanel = host.querySelector(".app-shell-screen-drawer") as HTMLElement | null;
  assert(closingPanel?.dataset.closing === "true", "closing sets data-closing='true' when transition is active");

  const trEv = new win.Event("transitionend", { bubbles: true });
  Object.defineProperty(trEv, "propertyName", { value: "transform" });
  await React.act(async () => { closingPanel!.dispatchEvent(trEv); });
  await flush();
  assert(host.querySelector(".app-shell-screen-drawer") === null, "drawer unmounts after transform transitionend");

  // Under prefers-reduced-motion: reduce, close completes immediately without waiting on transitionend.
  reducedMotion = true;
  try {
    await React.act(async () => { drive("drawer", "create", true, "narrow"); });
    await flush();
    const rmPanel = host.querySelector(".app-shell-screen-drawer") as HTMLElement | null;
    assert(rmPanel !== null, "drawer reopens before reduced-motion close check");
    rmPanel.style.transitionDuration = "0.2s";
    const rmClose = rmPanel.querySelector("button[aria-label='Close']") as HTMLElement;
    await React.act(async () => { rmClose.click(); });
    await flush();
    assert(host.querySelector(".app-shell-screen-drawer") === null, "prefers-reduced-motion closes drawer immediately");
  } finally {
    reducedMotion = false;
  }

  await React.act(async () => root.unmount());
  host.remove();
}

async function main() {
  await checkInitialActionsAndModelQuote();
  await checkAppShellDrawerTitleAndMotion();
  await win.happyDOM.close();
  console.log("CAD-2852 designerStudioV3Host tests passed");
}
void main();
