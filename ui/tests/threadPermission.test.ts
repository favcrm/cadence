/** CAD-1306: ThreadPermission itself renders the compact card (risk line, the
 *  rail's labels, the whole reason) and keeps WriteGate read-only. */
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
Object.defineProperty(globalThis, "fetch", {
  configurable: true,
  value: async () =>
    new Response(JSON.stringify({ requests: [request], rules: [] }), { status: 200, headers: { "Content-Type": "application/json" } }),
});
const { createElement, act } = require("react");
const { createRoot } = require("react-dom/client");
const { ThreadPermission } = require("../src/features/home/PermissionCard") as typeof import("../src/features/home/PermissionCard");

function ok(cond: unknown, what: string): void {
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
    await new Promise((r) => setTimeout(r, 20));
  });
  const card: any = host.querySelector(`[data-permission-thread="${ID}"]`);
  ok(card, "the thread card renders");
  ok(card.querySelector('[data-permission-risk="high"]'), "thread card shows the risk line");
  const buttons = [...card.querySelectorAll("button")] as HTMLButtonElement[];
  const labels = buttons.map((b) => b.textContent?.trim());
  ok(["Allow once", "Always ▾", "Deny ▾"].every((l) => labels.includes(l)), `thread card uses the rail's labels, got ${labels.join("|")}`);
  ok(card.textContent?.includes("Master says:") && card.textContent.includes("clean up the scratch folder"), "thread card shows the whole reason");
  ok(buttons.length === 3 && buttons.every((b) => b.disabled), "read-only disables every decision button");
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
