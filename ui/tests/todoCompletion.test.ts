export {};
/** CAD-1273: an old send must not clear a newer need's in-flight mark. */
declare function require(name: string): any;
const { Window } = require("happy-dom");
const win = new Window({ url: "http://localhost/" });
for (const name of ["window", "document", "navigator", "HTMLElement"])
  Object.defineProperty(globalThis, name, { value: name === "window" ? win : win[name], configurable: true });
Object.defineProperty(globalThis, "IS_REACT_ACT_ENVIRONMENT", { value: true });
const React = require("react") as typeof import("react");
const { createRoot } = require("react-dom/client");
const { resetTodoLocal, sendingTodo, settleTodo, marksFor, useTodoLocal } = require("../src/features/home/todoLocal");
const oldNeed = { key: "same-row", kind: "merge_decision", action: { type: "merge", issue: "CAD-1", sha: "a" } };
const newNeed = { ...oldNeed, action: { ...oldNeed.action, sha: "b" } };
const host = document.createElement("div");
const root = createRoot(host);
function Probe() {
  return React.createElement("span", null, marksFor(useTodoLocal(), newNeed).sending ? "sending" : "idle");
}
async function main() {
  try {
    await React.act(async () => root.render(React.createElement(Probe)));
    for (const ok of [false, true]) {
      await React.act(async () => { resetTodoLocal(); sendingTodo(oldNeed); sendingTodo(newNeed); settleTodo(oldNeed, ok); });
      if (host.textContent !== "sending") throw new Error(`old completion (${ok}) cleared the newer in-flight send`);
      await React.act(async () => settleTodo(newNeed, false));
      if (String(host.textContent) !== "idle") throw new Error("matching completion must release its own mark");
    }
    console.log("To-do completion ownership passed");
  } finally { await React.act(async () => root.unmount()); }
}
void main().catch((error: unknown) => { console.error(error); (globalThis as any).process.exitCode = 1; });
