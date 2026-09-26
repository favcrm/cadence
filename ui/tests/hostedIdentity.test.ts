import type { Meta } from "../src/lib/types";
import { composerBlock, type MasterStatus } from "../src/features/home/master";

declare function require(name: string): any;
const { Window } = require("happy-dom");
const win = new Window({ url: "http://localhost/" });
for (const key of ["window", "document", "navigator", "HTMLElement", "location"]) {
  Object.defineProperty(globalThis, key, { value: key === "window" ? win : win[key], configurable: true });
}
Object.defineProperty(globalThis, "IS_REACT_ACT_ENVIRONMENT", { value: true });
const { createElement, act } = require("react");
const { createRoot } = require("react-dom/client");
const { default: SignIn } = require("../src/features/auth/SignIn") as typeof import("../src/features/auth/SignIn");

function assert(condition: unknown, message: string): void {
  if (!condition) throw new Error(message);
}

const base = { read_only: false, signed_in: true, actor: "operator (ui)", tailnet_url: null, version: "0", build_commit: "x", build_time: "t" };
for (const role of ["member", "operator"]) {
  const host = document.createElement("div");
  document.body.append(host);
  const root = createRoot(host);
  const meta = { ...base, session: { id: "safe-session", origin: "public", user: { name: "Fable Chen", email: "fable@example.com", role } } } as unknown as Meta;
  act(() => root.render(createElement(SignIn, { meta, onChange: () => undefined })));
  const button = host.querySelector("button");
  assert(button?.textContent?.includes("Fable Chen"), "signed-in chip must name the person");
  assert(button?.title.includes("fable@example.com") && button.title.includes(role), "signed-in title must show email and mapped role");
  if (role === "member") assert(!button?.textContent?.includes("operator"), "member must never be labelled operator");
  act(() => root.unmount());
  host.remove();
}

const hostedBlock: (block: string | null, status: MasterStatus, hosted?: boolean) => string | null = composerBlock;
for (const status of [{ kind: "absent" }, { kind: "stopped", label: "stopped" }] as MasterStatus[]) {
  const copy = hostedBlock(null, status, true) ?? "";
  assert(!copy.includes("cadence") && !copy.includes("terminal"), "hosted state must not ask for host shell access");
  assert(/unavailable/i.test(copy), "hosted state must report unavailable without inventing a starting operation");
  assert(!/starting|retry/i.test(copy), "no imaginary startup or retry action");
}
assert(hostedBlock(null, { kind: "absent" }, false)?.includes("cadence master start"), "local startup command stays available");
console.log("hosted identity checks passed");
