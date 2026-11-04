/**
 * CAD-2091: what History says about who changed the instructions and when.
 * Native records the actor on the event; hosted records only that the profile
 * changed, so it must not claim an instructions change or a person.
 */
import type * as HappyDom from "happy-dom";
import type * as ReactNs from "react";
import type * as ReactDomClient from "react-dom/client";
import type AgentTabBodyType from "../src/features/agents/AgentTabs";
import type { TabContext } from "../src/features/agents/AgentTabs";

declare function require(name: string): unknown;
declare const process: { exit(code: number): never };
export {};

function assert(value: unknown, why: string): asserts value {
  if (!value) throw new Error(`instructionsHistory: ${why}`);
}
type ModuleLoader = { prototype: { require: (this: unknown, id: string) => unknown } };

async function main() {
  const { Window } = require("happy-dom") as typeof HappyDom;
  const win = new Window({ url: "http://localhost/" });
  for (const name of ["window", "document", "Node", "Element", "HTMLElement", "HTMLInputElement", "HTMLSelectElement", "HTMLTextAreaElement", "SVGElement", "navigator", "MutationObserver", "ResizeObserver", "Event", "MouseEvent", "KeyboardEvent", "location", "history", "sessionStorage", "localStorage"] as const)
    Object.defineProperty(globalThis, name, { value: name === "window" ? win : win[name], configurable: true, writable: true });
  for (const name of ["addEventListener", "removeEventListener", "requestAnimationFrame", "cancelAnimationFrame"] as const)
    Object.defineProperty(globalThis, name, { value: win[name].bind(win), configurable: true, writable: true });
  Object.defineProperty(globalThis, "IS_REACT_ACT_ENVIRONMENT", { value: true });
  const loader = require("module") as ModuleLoader;
  const originalRequire = loader.prototype.require;
  loader.prototype.require = function (this: unknown, id: string) {
    if (id.endsWith(".css")) return {};
    if (id === "@hugeicons/core-free-icons") return new Proxy({}, { get: () => ({}) });
    if (id === "@hugeicons/react") return { HugeiconsIcon: () => null };
    return originalRequire.apply(this, [id]);
  };
  const React = require("react") as typeof ReactNs;
  const { createRoot } = require("react-dom/client") as typeof ReactDomClient;
  const Body = (require("../src/features/agents/AgentTabs") as { default: typeof AgentTabBodyType }).default;

  const render = async (c: Partial<TabContext>, open = false) => {
    const host = document.createElement("div");
    document.body.appendChild(host);
    const root = createRoot(host);
    await React.act(async () => { root.render(React.createElement(Body, { tab: "history", c: c as TabContext })); });
    if (open) {
      const row = host.querySelector<HTMLButtonElement>("[data-record-id]");
      await React.act(async () => { row?.dispatchEvent(new MouseEvent("click", { bubbles: true })); });
    }
    const text = open ? (document.body.textContent ?? "") : (host.textContent ?? "");
    await React.act(async () => { root.unmount(); });
    return text;
  };

  const withEvents = (operator: boolean, by: boolean): Partial<TabContext> => ({
    operator,
    detailNotServed: false,
    detailFailed: false,
    detail: {
      agent: { alias: "a", provider: "claude", endpoint_kind: "tmux", state: "idle", enabled: true },
      queued: 0, unknown: 0, running: [], fenced: false,
      events: [
        { seq: 2, alias: "a", kind: "instructions_updated", payload: by ? { by: "operator" } : undefined, at: "2026-11-02T10:00:00Z" },
      ],
    } as never,
  });
  const native = await render(withEvents(true, true));
  assert(/Instructions edited/.test(native) && /By operator/.test(native), "native shows who edited the instructions");
  assert(/Recent events only/.test(native), "the recent-events note is always there, not only at 20 rows");
  const drawer = await render(withEvents(true, true), true);
  assert(/Changed by/.test(drawer) && !/\bBy\b(?! operator)/.test(drawer.replace(/By operator/g, "")), "the item drawer labels the actor with its own key");
  const unrecorded = await render(withEvents(true, false));
  assert(/Who isn’t recorded/.test(unrecorded), "the operator is told when an event has no actor");
  const member = await render(withEvents(false, false));
  assert(/Instructions edited/.test(member) && !/Who isn’t recorded/.test(member), "a member's events withhold the actor, so nothing says it isn't recorded");

  const hosted = await render({
    hired: { id: "ap_1", alias: "a" } as never,
    doc: { kind: "hosted", profile: { revision: 6, updated_at: 1730600000000 } } as never,
  });
  assert(/Profile last changed/.test(hosted) && /revision 6/.test(hosted), "hosted shows when the profile last changed");
  assert(/not who changed it or which field/.test(hosted), "and that who and which field are not recorded");
  assert(!/Instructions last changed/.test(hosted), "it does not claim an instructions change");
  console.log("instructionsHistory checks passed");
}
void main().catch((e: unknown) => { console.error(e); process.exit(1); });
