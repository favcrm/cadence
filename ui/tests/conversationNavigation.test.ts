/** Queue navigation uses the predecessor, including late ownership reads and Home's real ID. */
import type * as HappyDOM from "happy-dom";
import type * as ReactTypes from "react";
import type * as ReactDOM from "react-dom/client";
import type * as Navigation from "../src/features/app-shell/chat/conversationNavigation";
import type * as Visibility from "../src/features/app-shell/chat/chatVisibility";
import type * as Conversations from "../src/features/app-shell/conversationClient";
import type * as Turns from "../src/features/app-shell/chat/turnBarState";
declare function require(name: string): unknown;
// Runtime imports follow DOM installation; the namespace types retain module checking.
const { Window } = require("happy-dom") as typeof HappyDOM;
const win = new Window({ url: "http://localhost/" });
for (const name of ["window", "document", "navigator", "Node", "HTMLElement", "Event", "location", "localStorage", "sessionStorage"] as const)
  Object.defineProperty(globalThis, name, { value: name === "window" ? win : win[name], configurable: true });
Object.defineProperty(globalThis, "IS_REACT_ACT_ENVIRONMENT", { value: true });
const React = require("react") as typeof ReactTypes;
const { createRoot } = require("react-dom/client") as typeof ReactDOM;
const { useConversationDestination } = require("../src/features/app-shell/chat/conversationNavigation") as typeof Navigation;
const { setKnownInstalls } = require("../src/features/app-shell/chat/chatVisibility") as typeof Visibility;
const { conversationList } = require("../src/features/app-shell/conversationClient") as typeof Conversations;
const { turnBarState } = require("../src/features/app-shell/chat/turnBarState") as typeof Turns;
const json = (body: unknown) => new Response(JSON.stringify(body), { headers: { "Content-Type": "application/json" } });
globalThis.fetch = (async (input: unknown) => {
  const path = new URL(String(input), "http://localhost").pathname;
  const id = path.includes("install-b") ? "conv-b" : "conv-a";
  return json({ conversations: [{ id, title: id, is_general: true, updated: 100, reply_seq: 0 }] });
}) as typeof fetch;
const master = {
  alias: "master", provider: "fixture", endpoint_kind: "managed", live: true,
  turn: { state: "working" as const, message: "run-a", conversation: "conv-a", app_origin: { install_id: "install-a", context_id: "ctx-a", conversation_id: "conv-a" } },
  queue: [{ message: "queue-b", conversation: "conv-b" }, { message: "home-q", conversation: "home-thread" }],
};
function Destination({ conversation }: { conversation: string | null }) {
  const href = useConversationDestination(conversation, master, "home-thread");
  return React.createElement("a", { href: href ?? undefined }, "View");
}
function equal(actual: unknown, expected: unknown, why: string): void {
  if (actual !== expected) throw new Error(`${why}: expected ${expected}, got ${actual}`);
}
async function main() {
  setKnownInstalls(["install-a", "install-b"]);
  await conversationList("install-a").refresh();
  const state = turnBarState({ master, entries: [], pendingMessages: ["home-q"], awaitingApproval: false, reconnecting: false }, "home-thread");
  if (state.kind !== "queued") throw new Error("Home is queued behind B");
  const host = document.createElement("div");
  document.body.append(host);
  const root = createRoot(host);
  const render = async (conversation: string | null) => {
    await React.act(async () => { root.render(React.createElement(Destination, { conversation })); });
  };
  await render(state.aheadConversation);
  equal(host.querySelector("a")?.getAttribute("href"), null, "an unresolved predecessor never borrows running A's origin");
  await React.act(async () => { await conversationList("install-b").refresh(); });
  equal(host.querySelector("a")?.getAttribute("href"), "/app-installations/install-b?conversation=conv-b", "late ownership resolves the queued predecessor B, not running A");
  await render("home-thread");
  equal(host.querySelector("a")?.getAttribute("href"), "/", "a real Home thread ID opens Home");
  await render("conv-a");
  equal(host.querySelector("a")?.getAttribute("href"), "/app-installations/install-a?conversation=conv-a&ctx=ctx-a", "a matching verified running origin retains its context");
  await render("unknown-conversation");
  equal(host.querySelector("a")?.getAttribute("href"), null, "unknown conversations have no destination");
  await React.act(async () => { root.unmount(); });
  console.log("conversation navigation checks passed");
}
void main();
