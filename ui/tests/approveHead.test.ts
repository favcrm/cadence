export {};
/** CAD-1300: a member refusal must not stick after an authorized read. */
declare function require(name: string): any;
const { Window } = require("happy-dom");
const win = new Window({ url: "http://localhost/" });
for (const name of ["window", "document", "navigator", "HTMLElement", "location"])
  Object.defineProperty(globalThis, name, { value: name === "window" ? win : win[name], configurable: true });
Object.defineProperty(globalThis, "IS_REACT_ACT_ENVIRONMENT", { value: true });
const React = require("react") as typeof import("react");
const { createRoot } = require("react-dom/client") as typeof import("react-dom/client");
const { api, ApiError } = require("../src/lib/api");
const { useApproveHead } = require("../src/features/home/ApproveHead");
const host = document.createElement("div");
const root = createRoot(host);
let memberSession = true;
api.approvalState = async () => {
  if (memberSession) throw new ApiError("member", 403, { code: "member_role" });
  return { state: "missing" };
};
function Probe({ head }: { head: string }) {
  const approval = useApproveHead("https://github.com/acme/widgets/pull/42", head);
  return React.createElement("span", null, approval.member ? "member" : "owner");
}
async function main() {
  try {
    await React.act(async () => { root.render(React.createElement(Probe, { head: "a".repeat(40) })); });
    if (host.textContent !== "member") throw new Error("member refusal must disable owner actions");
    memberSession = false;
    await React.act(async () => { root.render(React.createElement(Probe, { head: "b".repeat(40) })); });
    if (String(host.textContent) !== "owner") throw new Error("successful owner read must clear the stale member restriction");
    console.log("approval member-to-owner recovery passed");
  } finally {
    await React.act(async () => root.unmount());
  }
}
void main().catch((error: unknown) => { console.error(error); (globalThis as any).process.exitCode = 1; });
