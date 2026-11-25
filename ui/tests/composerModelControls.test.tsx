/** CAD-2792 frontend checks: searchable Model select pill, 3-bar Thinking Effort pill, and cleaner composer footer. */
declare function require<T = unknown>(name: string): T;
declare const process: { exitCode?: number };

import type * as ReactModule from "react";
import type { Root } from "react-dom/client";
import type * as ComposerControlsModule from "../src/features/app-shell/chat/ComposerModelControls";
import type * as LocaleModule from "../src/lib/locale";
import type { MasterModels } from "../src/lib/types";

function assert(value: unknown, why: string): asserts value {
  if (!value) throw new Error(why);
}

async function main(): Promise<void> {
  const { Window } = require<{ Window: new (options: { url: string }) => Record<string, any> }>("happy-dom");
  const win = new Window({ url: "http://localhost/" });
  const pointerCtor = win.PointerEvent ?? win.MouseEvent;
  for (const name of [
    "window",
    "document",
    "Node",
    "Element",
    "HTMLElement",
    "HTMLButtonElement",
    "HTMLDialogElement",
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
    "localStorage",
  ]) {
    Object.defineProperty(globalThis, name, {
      value: name === "window" ? win : win[name],
      configurable: true,
      writable: true,
    });
  }
  Object.defineProperty(globalThis, "PointerEvent", { value: pointerCtor, configurable: true, writable: true });
  for (const name of ["addEventListener", "removeEventListener"]) {
    Object.defineProperty(globalThis, name, { value: win[name].bind(win), configurable: true });
  }
  Object.defineProperty(globalThis, "requestAnimationFrame", {
    value: (cb: (time: number) => void) => setTimeout(() => cb(Date.now()), 0),
    configurable: true,
  });
  Object.defineProperty(globalThis, "cancelAnimationFrame", {
    value: (id: ReturnType<typeof setTimeout>) => clearTimeout(id),
    configurable: true,
  });
  Object.defineProperty(globalThis, "IS_REACT_ACT_ENVIRONMENT", { value: true, configurable: true });

  const calls: Array<{ method: string; url: string; body?: unknown }> = [];
  let serverModelsPayload: MasterModels = {
    current: { model: "thinkingmachines/inkling-2-mini", effort: "low" },
    models: [
      {
        id: "deepseek/deepseek-v4.1-flash-1017",
        label: "DeepSeek V4.1 Flash",
        is_default: true,
        efforts: [],
        default_effort: null,
        cost_tier: "Low cost",
      },
      {
        id: "thinkingmachines/inkling-2-mini",
        label: "Inkling 2 Mini",
        is_default: false,
        efforts: ["low", "medium", "high"],
        default_effort: "low",
        cost_tier: "Paid",
      },
    ],
    efforts: ["low", "medium", "high"],
  };

  Object.defineProperty(globalThis, "fetch", {
    value: async (input: string | URL, init?: { method?: string; body?: string }) => {
      const url = typeof input === "string" ? input : input.toString();
      const method = init?.method ?? "GET";
      const parsedBody = init?.body ? (JSON.parse(init.body) as unknown) : undefined;
      calls.push({ method, url, body: parsedBody });
      if (url.includes("/api/master/models")) {
        return new Response(JSON.stringify(serverModelsPayload), {
          status: 200,
          headers: { "content-type": "application/json" },
        });
      }
      if (url.includes("/api/master/command")) {
        return new Response(JSON.stringify({ ok: true, result: "Updated." }), {
          status: 200,
          headers: { "content-type": "application/json" },
        });
      }
      return new Response(JSON.stringify({}), {
        status: 200,
        headers: { "content-type": "application/json" },
      });
    },
    configurable: true,
    writable: true,
  });

  const loader = require<{ prototype: { require: (this: unknown, id: string) => unknown } }>("module");
  const originalRequire = loader.prototype.require;
  loader.prototype.require = function (this: unknown, id: string) {
    if (id.endsWith(".css")) return {};
    return originalRequire.call(this, id);
  };

  const React = require<typeof ReactModule>("react");
  const { createRoot } = require<{ createRoot: (host: Element) => Root }>("react-dom/client");
  const ComposerModelControls = require<typeof ComposerControlsModule>(
    "../src/features/app-shell/chat/ComposerModelControls",
  ).default;
  const { LocaleProvider } = require<typeof LocaleModule>("../src/lib/locale");

  const host: Element = win.document.createElement("div");
  win.document.body.appendChild(host);
  const root = createRoot(host);
  const flush = async () => {
    await React.act(async () => {
      await new Promise((r) => setTimeout(r, 10));
    });
  };
  const masterBase = {
    alias: "master" as const,
    provider: "cadence",
    endpoint_kind: "durable_object",
    live: true,
    identity: { name: "Assistant", tone: null, look: "robot" as const, revision: "1" },
  };

  // 1. Render with reasoning-capable active model ("Inkling 2 Mini" @ "low").
  await React.act(async () => {
    root.render(
      React.createElement(
        LocaleProvider,
        null,
        React.createElement(ComposerModelControls, {
          master: {
            ...masterBase,
            model: "thinkingmachines/inkling-2-mini",
            model_label: "thinkingmachines/inkling-2-mini",
            effort: "low",
            commands: ["new", "stop", "model", "effort"],
          },
          readOnly: false,
          canPickModel: true,
          canPickEffort: true,
          onRefreshState: () => undefined,
        }),
      ),
    );
  });
  await flush();

  const modelBtn = host.querySelector("[data-composer-model]") as HTMLButtonElement | null;
  const effortBtn = host.querySelector("[data-composer-effort]") as HTMLButtonElement | null;
  assert(modelBtn !== null && effortBtn !== null, "renders both model and effort pills");
  assert(modelBtn.textContent?.includes("Inkling 2 Mini"), `model pill shows friendly label (got ${modelBtn.textContent})`);
  assert(!effortBtn.textContent?.includes("Effort:"), `effort pill omits Effort: prefix (got ${effortBtn.textContent})`);
  assert(effortBtn.textContent?.includes("Low"), `effort pill shows short label Low (got ${effortBtn.textContent})`);
  assert(
    effortBtn.querySelector(".effort-meter")?.getAttribute("data-bars") === "1",
    "low effort lights 1 bar on 3-bar meter",
  );

  // 2. Open model popover: sticky search filters items, Default-only badge shown, NO speed/cost chips.
  await React.act(async () => {
    modelBtn.click();
  });
  await flush();

  const search = host.querySelector("[data-composer-model-search]") as HTMLInputElement | null;
  assert(search !== null, "model menu renders sticky search filter");
  const allItemsText = Array.from(host.querySelectorAll(".composer-pop-item")).map(
    (b) => b.textContent ?? "",
  );
  assert(allItemsText.length === 2, `renders 2 model rows (got ${allItemsText.length})`);
  assert(
    allItemsText[0].includes("Default") && !allItemsText[1].includes("Default"),
    "renders Default badge only on default model",
  );
  assert(
    host.querySelectorAll(".composer-default-badge").length === 1,
    "renders a single Default badge element across model rows",
  );
  for (const forbidden of ["Fast", "Balanced", "Light", "Low cost", "Paid"]) {
    assert(
      !allItemsText.some((txt) => txt.includes(forbidden)),
      `must not render forbidden badge/tier "${forbidden}"`,
    );
  }

  const inputSetter = Object.getOwnPropertyDescriptor(win.HTMLInputElement.prototype, "value")!.set!;
  await React.act(async () => {
    inputSetter.call(search, "deepseek");
    search.dispatchEvent(new Event("input", { bubbles: true }));
  });
  await flush();

  const filteredItems = Array.from(host.querySelectorAll(".composer-pop-item")) as HTMLButtonElement[];
  assert(filteredItems.length === 1, `search narrows to 1 model (got ${filteredItems.length})`);

  // Click filtered DeepSeek item -> dispatches POST /api/master/command { command: "model", arg: "deepseek/..." }.
  await React.act(async () => {
    filteredItems[0].click();
  });
  await flush();
  assert(
    calls.some(
      (c) =>
        c.method === "POST" &&
        c.url.includes("/api/master/command") &&
        JSON.stringify(c.body) === JSON.stringify({ command: "model", arg: "deepseek/deepseek-v4.1-flash-1017" }),
    ),
    "picking model posts master command model",
  );

  // Land the model pick via updated master state so pending clears, then switch back to a reasoning model.
  await React.act(async () => {
    root.render(
      React.createElement(
        LocaleProvider,
        null,
        React.createElement(ComposerModelControls, {
          master: {
            ...masterBase,
            model: "deepseek/deepseek-v4.1-flash-1017",
            model_label: "DeepSeek V4.1 Flash",
            effort: null,
            commands: ["new", "stop", "model", "effort"],
          },
          readOnly: false,
          canPickModel: true,
          canPickEffort: true,
          onRefreshState: () => undefined,
        }),
      ),
    );
  });
  await flush();
  await React.act(async () => {
    root.render(
      React.createElement(
        LocaleProvider,
        null,
        React.createElement(ComposerModelControls, {
          master: {
            ...masterBase,
            model: "thinkingmachines/inkling-2-mini",
            model_label: "Inkling 2 Mini",
            effort: "low",
            commands: ["new", "stop", "model", "effort"],
          },
          readOnly: false,
          canPickModel: true,
          canPickEffort: true,
          onRefreshState: () => undefined,
        }),
      ),
    );
  });
  await flush();

  // 3. Open effort popover and pick high -> dispatches { command: "effort", arg: "high" }.
  const currentEffortBtn = host.querySelector("[data-composer-effort]") as HTMLButtonElement | null;
  assert(currentEffortBtn !== null, "reasoning model renders effort pill");
  await React.act(async () => {
    currentEffortBtn.click();
  });
  await flush();
  const effortItems = Array.from(host.querySelectorAll(".composer-pop-item")) as HTMLButtonElement[];
  const highBtn = effortItems.find((b) => b.textContent?.includes("High"));
  assert(highBtn !== undefined, "effort menu lists High");
  await React.act(async () => {
    highBtn.click();
  });
  await flush();
  assert(
    calls.some(
      (c) =>
        c.method === "POST" &&
        c.url.includes("/api/master/command") &&
        JSON.stringify(c.body) === JSON.stringify({ command: "effort", arg: "high" }),
    ),
    "picking effort posts master command effort",
  );

  // 4. Switching to a model with zero supportedEfforts hides the effort pill automatically.
  serverModelsPayload = {
    ...serverModelsPayload,
    current: { model: "deepseek/deepseek-v4.1-flash-1017", effort: null },
    efforts: [],
  };
  await React.act(async () => {
    root.render(
      React.createElement(
        LocaleProvider,
        null,
        React.createElement(ComposerModelControls, {
          master: {
            ...masterBase,
            model: "deepseek/deepseek-v4.1-flash-1017",
            model_label: "deepseek/deepseek-v4.1-flash-1017",
            effort: null,
            commands: ["new", "stop", "model", "effort"],
          },
          readOnly: false,
          canPickModel: true,
          canPickEffort: true,
          onRefreshState: () => undefined,
        }),
      ),
    );
  });
  await flush();
  assert(
    host.querySelector("[data-composer-effort]") === null,
    "effort pill hides when active model exposes no supported efforts",
  );

  // 5. Hidden completely when canPickModel is false (non-owner member).
  await React.act(async () => {
    root.render(
      React.createElement(
        LocaleProvider,
        null,
        React.createElement(ComposerModelControls, {
          master: {
            ...masterBase,
            model: "thinkingmachines/inkling-2-mini",
            effort: "low",
            commands: ["new", "stop"],
          },
          readOnly: false,
          canPickModel: false,
          canPickEffort: false,
        }),
      ),
    );
  });
  await flush();
  assert(
    host.querySelector("[data-composer-model]") === null &&
      host.querySelector("[data-composer-effort]") === null,
    "renders nothing when canPickModel is false",
  );

  await React.act(async () => {
    root.unmount();
  });
  console.log("composerModelControls checks passed");
}

main().catch((error) => {
  console.error(error);
  process.exitCode = 1;
});
