/** CAD-1306: ThreadPermission itself renders the compact card (risk line, the
 *  rail's labels, the whole reason) and keeps WriteGate read-only. */
import type * as ReactModule from "react";
import type * as MasterChanges from "../src/lib/masterChanged";
import type * as PendingApproval from "../src/features/app-shell/chat/usePendingApproval";
import type { ThreadEntry } from "../src/features/home/thread";
export {};
declare function require(name: string): any;
declare const process: { exit(code: number): never };
const { Window } = require("happy-dom");
const win = new Window({ url: "http://localhost/" });
for (const key of ["window", "document", "navigator", "HTMLElement", "Node", "Event", "location", "localStorage"]) {
  Object.defineProperty(globalThis, key, { value: key === "window" ? win : win[key], configurable: true });
}
Object.defineProperty(globalThis, "IS_REACT_ACT_ENVIRONMENT", { value: true });
const ID = "mp-0123abcd4567";
const request = { id: ID, command: "rm -rf /tmp/x", cwd: "/tmp", reason: "clean up the scratch folder", risk: "high", status: "pending", expires_at: 9999999999, prefix: null };
const requests = [request];
let deferRules = false;
const held: Array<() => void> = [];
const { setImmediate: nextTurn } = require("node:timers/promises") as { setImmediate: () => Promise<void> };
Object.defineProperty(globalThis, "fetch", {
  configurable: true,
  value: async () => {
    const response = new Response(JSON.stringify({ requests, rules: [] }), { status: 200, headers: { "Content-Type": "application/json" } });
    return deferRules ? new Promise<Response>((resolve) => held.push(() => resolve(response))) : response;
  },
});
const { createElement, act } = require("react") as typeof ReactModule;
const { createRoot } = require("react-dom/client");
const { ThreadPermission } = require("../src/features/home/PermissionCard") as typeof import("../src/features/home/PermissionCard");
const { noteMasterChanged } = require("../src/lib/masterChanged") as typeof MasterChanges;
const { usePendingPermission } = require("../src/features/app-shell/chat/usePendingApproval") as typeof PendingApproval;
const entries: ThreadEntry[] = [{ seq: 1, role: "system", kind: "message", text: `Permission requested (${ID}, risk high): command`, payload: { source: "permission" } }];
function Pending() {
  return createElement("output", { "data-pending": String(usePendingPermission(entries)) });
}

function ok(cond: unknown, what: string): asserts cond {
  if (!cond) throw new Error(`failed: ${what}`);
}

async function main(): Promise<void> {
  const host = document.createElement("div");
  document.body.append(host);
  const root = createRoot(host);
  await act(async () => {
    root.render(createElement(ThreadPermission, { text: `Permission requested (${ID}, risk high): \`rm -rf /tmp/x\``, readOnly: true }));
  });
  await act(async () => {
    await nextTurn();
  });
  const card = host.querySelector<HTMLElement>(`[data-permission-thread="${ID}"]`);
  ok(card, "the thread card renders");
  ok(card.querySelector('[data-permission-risk="high"]'), "thread card shows the risk line");
  const buttons = [...card.querySelectorAll("button")] as HTMLButtonElement[];
  const labels = buttons.map((b) => b.textContent?.trim());
  ok(["Allow once", "Always ▾", "Deny ▾"].every((l) => labels.includes(l)), `thread card uses the rail's labels, got ${labels.join("|")}`);
  ok(card.textContent?.includes("Master says:") && card.textContent.includes("clean up the scratch folder"), "thread card shows the whole reason");
  ok(buttons.length === 3 && buttons.every((b) => b.disabled), "read-only disables every decision button");
  // A decision in another tab arrives through the pushed master event,
  // not this tab's local permission publication.
  request.status = "allowed_once";
  await act(async () => { noteMasterChanged(); await nextTurn(); });
  ok(card.querySelector('[data-permission-status="allowed_once"]'), "a pushed remote decision retires the mounted pending card");

  // An older in-flight read cannot restore a decided card or its Review bar.
  request.status = "pending";
  await act(async () => {
    root.render(createElement("main", null,
      createElement(ThreadPermission, { text: entries[0].text, readOnly: true }),
      createElement(Pending)));
    await nextTurn();
  });
  ok(host.querySelector('[data-pending="true"]'), "the new request is actionable");
  deferRules = true;
  await act(async () => { noteMasterChanged(); await nextTurn(); });
  request.status = "denied";
  deferRules = false;
  await act(async () => { noteMasterChanged(); await nextTurn(); });
  ok(host.querySelector('[data-permission-status="denied"]') && host.querySelector('[data-pending="false"]'), "the latest decision clears both card and Review state");
  await act(async () => { held.splice(0).forEach((resolve) => resolve()); await nextTurn(); });
  ok(!host.querySelector('[data-permission-status="pending"]') && host.querySelector('[data-pending="false"]'), "late pending reads cannot resurrect a decided request");
  act(() => root.unmount());
  console.log("thread permission checks passed");
}
main().then(
  () => process.exit(0),
  (e) => {
    console.error(e);
    process.exit(1);
  },
);
