/** Empty/operator/tool writes are not replies; real replies notify and clear across tabs. */
import type * as HappyDOM from "happy-dom";
import type * as ReactModule from "react";
import type * as ReactDOM from "react-dom/client";
import type * as ShellUnread from "../src/features/app-shell/chat/unreadShell";
import type * as HomeUnread from "../src/features/app-shell/chat/homeUnread";
import type * as Unread from "../src/features/app-shell/chat/assistantUnread";
import type * as Seen from "../src/features/app-shell/chat/unreadSeen";
import type * as Visibility from "../src/features/app-shell/chat/chatVisibility";
import type * as Toast from "../src/features/threads/NotificationToast";
import type * as Conversations from "../src/features/app-shell/conversationClient";
import type * as Changes from "../src/lib/masterChanged";
import type * as Resources from "../src/lib/resources";
import type { ThreadEntry } from "../src/features/home/thread";
declare function require<T>(name: string): T;
const { setImmediate: nextTurn } = require<{ setImmediate: () => Promise<void> }>("node:timers/promises");
const { Window } = require<typeof HappyDOM>("happy-dom");
const win = new Window({ url: "http://localhost/apps/crm" });
for (const name of ["window", "document", "navigator", "Node", "HTMLElement", "Event", "MouseEvent", "StorageEvent", "location", "history", "localStorage", "sessionStorage"] as const)
  Object.defineProperty(globalThis, name, { value: name === "window" ? win : win[name], configurable: true });
Object.defineProperty(globalThis, "IS_REACT_ACT_ENVIRONMENT", { value: true });
type Loader = { prototype: { require: (id: string) => unknown } };
const loader = require<Loader>("module");
const nativeRequire = loader.prototype.require;
loader.prototype.require = function (id: string): unknown {
  if (id.endsWith(".css")) return {};
  if (id === "@hugeicons/core-free-icons") return new Proxy({}, { get: () => ({}) });
  if (id === "@hugeicons/react") return { HugeiconsIcon: () => null };
  return nativeRequire.call(this, id);
};
const React = require<typeof ReactModule>("react");
const { createRoot } = require<typeof ReactDOM>("react-dom/client");
const { AssistantUnreadProbes } = require<typeof ShellUnread>("../src/features/app-shell/chat/unreadShell");
const { useHomeUnread, noteHomeSeen } = require<typeof HomeUnread>("../src/features/app-shell/chat/homeUnread");
const { useAssistantUnread } = require<typeof Unread>("../src/features/app-shell/chat/assistantUnread");
const { seenKey, writeSeen } = require<typeof Seen>("../src/features/app-shell/chat/unreadSeen");
const { setVisibleConversation } = require<typeof Visibility>("../src/features/app-shell/chat/chatVisibility");
const { NotificationToastProvider } = require<typeof Toast>("../src/features/threads/NotificationToast");
const { conversationList } = require<typeof Conversations>("../src/features/app-shell/conversationClient");
const { noteMasterChanged } = require<typeof Changes>("../src/lib/masterChanged");
const { resources } = require<typeof Resources>("../src/lib/resources");
let updated = 100;
let replySeq = 0;
let homeEntries: ThreadEntry[] = [];
const json = (body: unknown) => new Response(JSON.stringify(body), { headers: { "Content-Type": "application/json" } });
globalThis.fetch = async (input) => {
  const url = new URL(String(input), "http://localhost");
  if (url.pathname.endsWith("/conversations"))
    return json({ conversations: [{ id: "general", is_general: true, updated, reply_seq: replySeq }] });
  if (url.pathname === "/api/threads/master")
    return json({ thread: { id: "real-home", alias: "master" }, entries: homeEntries, more_before: false });
  if (url.pathname === "/api/master/state")
    return json({ alias: "master", provider: "fixture", endpoint_kind: "managed", live: true, turn: null, queue: [] });
  throw new Error(`Unexpected fixture request ${url.pathname}`);
};
const apps = [{ installId: "install-crm" }];
function Observe({ onHome }: { onHome: boolean }) {
  useHomeUnread(onHome);
  const unread = useAssistantUnread();
  return React.createElement(React.Fragment, null,
    React.createElement("nav", null,
      React.createElement("a", { href: "/", "aria-label": unread.has("home") ? "Home new reply" : "Home" }, "Home"),
      React.createElement("a", { href: "/apps/crm", "aria-label": unread.has("install-crm") ? "CRM new reply" : "CRM" }, "CRM")),
    React.createElement(AssistantUnreadProbes, { apps }));
}
function assert(condition: unknown, message: string): asserts condition {
  if (!condition) throw new Error(message);
}
async function main() {
  const host = document.createElement("div");
  document.body.append(host);
  const root = createRoot(host);
  const settle = async () => {
    for (let n = 0; n < 5; n++) await React.act(async () => { await nextTurn(); });
  };
  const render = async (onHome = false) => {
    await React.act(async () => { root.render(React.createElement(NotificationToastProvider, null, React.createElement(Observe, { onHome }))); });
    await settle();
  };
  const dots = () => host.querySelectorAll('nav [aria-label$="new reply"]').length;
  const notices = () => host.querySelectorAll(".notification-toast").length;
  const dismiss = async () => {
    await React.act(async () => {
      for (const button of host.querySelectorAll<HTMLButtonElement>('button[aria-label="Dismiss notification"]')) button.click();
    });
  };
  await render();
  assert(dots() === 0 && notices() === 0, "a newly created empty General conversation is not an unseen reply");
  updated = 200;
  await React.act(async () => { await conversationList("install-crm").refresh(); });
  await settle();
  assert(dots() === 0 && notices() === 0, "operator sends and tool writes do not raise reply signals");
  replySeq = 7;
  updated = 300;
  await React.act(async () => { await conversationList("install-crm").refresh(); });
  await settle();
  assert(host.querySelector('[aria-label="CRM new reply"]') && notices() === 1, "an offscreen app reply raises one nav dot and toast");
  await React.act(async () => { await conversationList("install-crm").refresh(); });
  await settle();
  assert(notices() === 1, "re-reading the same reply does not add another toast");
  const key = seenKey("install-crm");
  await React.act(async () => {
    win.localStorage.setItem(key, JSON.stringify({ general: 7 }));
    win.dispatchEvent(new win.StorageEvent("storage", { key, newValue: JSON.stringify({ general: 7 }) }));
  });
  await settle();
  assert(dots() === 0, "opening the app reply in another tab clears this nav dot");
  await dismiss();
  setVisibleConversation("install-crm", "general");
  writeSeen("install-crm", "general", 8);
  replySeq = 8;
  updated = 400;
  await React.act(async () => { await conversationList("install-crm").refresh(); });
  await settle();
  assert(dots() === 0 && notices() === 0, "a reply already visible in the pane is not announced");
  homeEntries = [{ seq: 20, role: "system", kind: "tool_result", text: "A tool step", message: "home-work" }];
  await React.act(async () => { noteMasterChanged(); await resources.masterThread.refresh(); });
  await settle();
  assert(!host.querySelector('[aria-label="Home new reply"]'), "Home tool/system entries are not assistant replies");
  homeEntries.push({ seq: 21, role: "agent", kind: "turn_result", text: "Home answer", message: "home-work" });
  await React.act(async () => { noteMasterChanged(); await resources.masterThread.refresh(); });
  await settle();
  assert(host.querySelector('[aria-label="Home new reply"]') && notices() === 1, "an offscreen Home reply has both a dot and a toast");
  const open = Array.from(host.querySelectorAll<HTMLButtonElement>(".notification-toast button")).find((button) => button.textContent === "Open");
  assert(open, "the Home reply toast has an Open action");
  await React.act(async () => { open.click(); });
  assert(location.pathname === "/", "the Home toast opens Home, not an installation");
  await React.act(async () => { noteHomeSeen(21); });
  await render(true);
  assert(dots() === 0, "viewing Home clears its reply state");
  await React.act(async () => { root.unmount(); });
  await win.happyDOM.close();
  console.log("reply notification checks passed");
}
void main();
