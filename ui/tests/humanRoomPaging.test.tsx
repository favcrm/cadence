import React from "react";
import { createRoot } from "react-dom/client";
import { Window } from "happy-dom";
import { LocaleProvider } from "../src/lib/locale";
import { HumanRoomTimeline } from "../src/features/threads/HumanThreads";

function assert(value: unknown, message: string): asserts value {
  if (!value) throw new Error(message);
}

async function main() {
  const win = new Window({ url: "http://localhost/threads" });
  for (const name of ["window", "document", "Node", "Element", "HTMLElement", "HTMLInputElement", "HTMLTextAreaElement", "HTMLSelectElement", "SVGElement", "navigator", "MutationObserver", "ResizeObserver", "Event", "MouseEvent", "KeyboardEvent", "location", "history", "sessionStorage", "localStorage"] as const) {
    Object.defineProperty(globalThis, name, { value: name === "window" ? win : win[name], configurable: true, writable: true });
  }
  for (const name of ["addEventListener", "removeEventListener", "requestAnimationFrame", "cancelAnimationFrame"] as const) {
    Object.defineProperty(globalThis, name, { value: win[name].bind(win), configurable: true });
  }
  Object.defineProperty(globalThis, "IS_REACT_ACT_ENVIRONMENT", { value: true });
  const seen: string[] = [];
  const room = "room-1";
  const members = [{ kind: "human", id: "human-1", label: "Avery" }];
  const reply = (data: unknown) => new Response(JSON.stringify(data), { headers: { "Content-Type": "application/json" } });
  globalThis.fetch = (async (input: unknown) => {
    const url = new URL(String(input), "http://localhost");
    if (url.pathname === `/api/human-threads/rooms/${room}`) {
      const before = url.searchParams.get("before");
      seen.push(before ?? "latest");
      if (before === "500") return reply({ room, members, posts: [], cursor: 250, has_more: true });
      if (before === "250") return reply({ room, members, posts: [{ id: "p1", seq: 1, sender: { id: "human-1", label: "Avery" }, body: "Visible earlier message", created: 1 }], cursor: 1, has_more: false });
      return reply({ room, members, posts: [], cursor: 500, has_more: true });
    }
    if (url.pathname.endsWith("/agents")) return reply({ agents: [] });
    return reply({ requests: [], pending: [], items: [] });
  }) as typeof fetch;
  const host = document.createElement("div");
  document.body.append(host);
  const root = createRoot(host);
  const onState = () => undefined;
  const onOpenContext = () => undefined;
  const onChanged = () => undefined;
  const render = (refresh: number) => root.render(<LocaleProvider><HumanRoomTimeline id={room} me={{ id: "human-1", name: "Avery" }} refresh={refresh} onState={onState} onOpenContext={onOpenContext} onChanged={onChanged} isDm /></LocaleProvider>);
  const flush = async () => { await React.act(async () => { await new Promise((resolve) => setTimeout(resolve, 0)); }); };
  const clickEarlier = async () => {
    const button = [...document.querySelectorAll("button")].find((item) => item.textContent?.trim() === "Earlier");
    assert(button, "an empty filtered page with a continuation must offer Earlier");
    await React.act(async () => button.dispatchEvent(new MouseEvent("click", { bubbles: true })));
    await flush();
  };
  await React.act(async () => render(0));
  await flush();
  await clickEarlier();
  assert(seen.includes("500"), "the first click uses the server continuation even with no visible posts");
  await React.act(async () => render(1));
  await flush();
  await clickEarlier();
  assert(seen.includes("250"), `refresh must preserve the oldest continuation, got ${seen}`);
  assert(host.textContent?.includes("Visible earlier message"), "older visible human posts remain reachable");
  assert(![...document.querySelectorAll("button")].some((item) => item.textContent?.trim() === "Earlier"), "the control disappears when history is exhausted");
  await React.act(async () => root.unmount());
  console.log("human room filtered paging checks passed");
}

void main();
