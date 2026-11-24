/** CAD-2778 frontend checks: hosted `humanAgentRooms` client and `AgentChannels` component. */
declare function require<T = unknown>(name: string): T;

import type * as ReactModule from "react";
import type { Root } from "react-dom/client";
import type * as ClientModule from "../src/features/threads/humanThreadsClient";
import type * as ChannelsModule from "../src/features/agents/AgentChannels";
import type * as ApiModule from "../src/lib/api";

function assert(cond: unknown, msg: string): asserts cond {
  if (!cond) throw new Error(msg);
}

function equal(actual: unknown, expected: unknown, msg: string): void {
  const a = JSON.stringify(actual);
  const e = JSON.stringify(expected);
  if (a !== e) throw new Error(`${msg}: expected ${e}, got ${a}`);
}

async function flushUi(): Promise<void> {
  await new Promise((resolve) => setTimeout(resolve, 15));
}

async function main(): Promise<void> {
  const { Window } = require<{ Window: new (options: { url: string }) => Record<string, any> }>("happy-dom");
  const win = new Window({ url: "http://127.0.0.1/agents/builder" });
  for (const name of [
    "window",
    "document",
    "Node",
    "Element",
    "HTMLElement",
    "HTMLAnchorElement",
    "HTMLButtonElement",
    "SVGElement",
    "navigator",
    "MutationObserver",
    "Event",
    "MouseEvent",
    "KeyboardEvent",
    "CustomEvent",
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
  for (const name of ["addEventListener", "removeEventListener"]) {
    Object.defineProperty(globalThis, name, { value: win[name].bind(win), configurable: true });
  }
  Object.defineProperty(globalThis, "IS_REACT_ACT_ENVIRONMENT", { value: true, configurable: true });

  const loader = require<{ prototype: { require: (this: unknown, id: string) => unknown } }>("module");
  const originalRequire = loader.prototype.require;
  loader.prototype.require = function (this: unknown, id: string) {
    if (id.endsWith(".css")) return {};
    return originalRequire.call(this, id);
  };

  const React = require<typeof ReactModule>("react");
  const { createRoot } = require<{ createRoot: (host: Element) => Root }>("react-dom/client");
  const { ApiError } = require<typeof ApiModule>("../src/lib/api");
  const { humanAgentRooms } = require<typeof ClientModule>("../src/features/threads/humanThreadsClient");
  const { AgentChannels } = require<typeof ChannelsModule>("../src/features/agents/AgentChannels");
  const { NavigationProvider } = require<typeof import("../screen-ui/src/navigation")>("../screen-ui/src/navigation");
  const { hostNavigation } = require<typeof import("../src/ui/hostNavigation")>("../src/ui/hostNavigation");

  const sampleRooms: ClientModule.HumanRoomSummary[] = [
    {
      id: "room-alpha/1",
      kind: "mixed_group",
      title: "release-ops",
      creator: "human-1",
      created: 100,
      unread: 3,
    },
    {
      id: "room-beta",
      kind: "mixed_group",
      title: "support-triage",
      creator: "human-1",
      created: 90,
      unread: 0,
    },
  ];

  // 1. humanAgentRooms fetch behavior: 200 -> rooms, 404/501 -> [], 500 -> throws ApiError.
  const calls: string[] = [];
  let responder: (url: string) => { status: number; body: unknown } = () => ({
    status: 200,
    body: { rooms: sampleRooms },
  });
  Object.defineProperty(globalThis, "fetch", {
    value: async (url: string) => {
      calls.push(url);
      const res = responder(url);
      return {
        ok: res.status >= 200 && res.status < 300,
        status: res.status,
        statusText: String(res.status),
        json: async () => res.body,
      };
    },
    configurable: true,
    writable: true,
  });

  const got200 = await humanAgentRooms("ap_1/special");
  equal(calls, ["/api/human-threads/agents/ap_1%2Fspecial/rooms"], "encodes uid in request URL");
  equal(got200, sampleRooms, "200 returns parsed rooms");

  responder = () => ({ status: 404, body: { error: { code: "not_found", message: "Not found" } } });
  equal(await humanAgentRooms("ap_1"), [], "404 degrades to empty array");

  responder = () => ({ status: 501, body: { error: { code: "not_implemented", message: "No channels" } } });
  equal(await humanAgentRooms("ap_1"), [], "501 degrades to empty array");

  responder = () => ({ status: 500, body: { error: { code: "internal", message: "Boom" } } });
  let threwApiError = false;
  try {
    await humanAgentRooms("ap_1");
  } catch (err) {
    threwApiError = err instanceof ApiError && err.status === 500;
  }
  assert(threwApiError, "500 propagates ApiError instead of masking failure as empty");

  // 2. AgentChannels rendering: links, unread count, empty state, and error state.
  const host = document.createElement("div");
  document.body.appendChild(host);
  const root = createRoot(host);
  const renderChannels = async (element: ReactModule.ReactElement) => {
    await React.act(async () => {
      root.render(React.createElement(NavigationProvider, { adapter: hostNavigation, children: element }));
      await flushUi();
    });
  };

  await renderChannels(
    React.createElement(AgentChannels, {
      profileId: "ap_1",
      readRooms: async () => sampleRooms,
    }),
  );

  const links = Array.from(host.querySelectorAll<HTMLAnchorElement>('[data-work-panel="agent-channels"] a.lnk'));
  equal(links.length, 2, "renders both shared channels");
  equal(
    links.map((a) => [a.getAttribute("href"), a.textContent]),
    [
      ["/threads?h=room-alpha%2F1", "# release-ops"],
      ["/threads?h=room-beta", "# support-triage"],
    ],
    "channel links point to /threads?h=<encoded room id>",
  );
  assert(host.textContent?.includes("3 unread"), "renders unread count when > 0");

  await renderChannels(
    React.createElement(AgentChannels, {
      profileId: "ap_empty",
      readRooms: async () => [],
    }),
  );
  assert(
    host.textContent?.includes("Not in any shared channels with you yet."),
    "empty list shows empty state",
  );

  await renderChannels(
    React.createElement(AgentChannels, {
      profileId: "ap_err",
      readRooms: async () => {
        throw new ApiError("Channels failed", 500);
      },
    }),
  );
  assert(
    host.textContent?.includes("Channels could not be loaded."),
    "rejected read shows error state",
  );

  root.unmount();
  console.log("agentChannels checks passed");
}

void main();
