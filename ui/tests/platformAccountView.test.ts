export {};
/** Account states keep a useful refresh path and distinguish saved data from fresh data. */
declare function require(name: string): any;
const { Window } = require("happy-dom");
const win = new Window({ url: "http://localhost/settings/account" });
for (const name of [
  "window", "document", "Node", "Element", "HTMLElement", "SVGElement",
  "navigator", "MutationObserver", "Event", "MouseEvent", "location", "history",
]) {
  Object.defineProperty(globalThis, name, {
    value: name === "window" ? win : win[name], configurable: true, writable: true,
  });
}
Object.defineProperty(globalThis, "IS_REACT_ACT_ENVIRONMENT", { value: true });
const loader = require("module");
const originalRequire = loader.prototype.require;
loader.prototype.require = function (this: unknown, id: string) {
  if (id.endsWith(".css")) return {};
  if (id === "@hugeicons/core-free-icons") return new Proxy({}, { get: () => ({}) });
  if (id === "@hugeicons/react") return { HugeiconsIcon: () => null };
  return originalRequire.apply(this, arguments);
};
const React = require("react") as typeof import("react");
const { createRoot } = require("react-dom/client") as typeof import("react-dom/client");
const Account = (require("../src/features/settings/PlatformAccount") as typeof import("../src/features/settings/PlatformAccount")).default;
type AccountData = import("../src/features/settings/accountDisplay").PlatformAccount;
const connected: AccountData = {
  configured: true,
  manage_url: "https://app-v2.agenticos.hk/account?company=acme",
  account: {
    company: { name: "Acme" },
    balance: { amount: "123.400", currency: "HKD", low: false, zero: false },
    plan: { name: "Team", status: "active" },
  },
  usage: [{ date: "2026-09-27T00:00:00Z", description: "Agent work", amount: "1.200", kind: "usage" }],
  account_error: null,
  usage_error: null,
};
const responses: Array<AccountData | "fail"> = [connected, "fail", {
  ...connected,
  account: null,
  account_error: "Balance is temporarily unavailable.",
}, {
  ...connected,
  usage_error: "Usage is temporarily unavailable.",
}, {
  configured: false, manage_url: null, account: null, usage: [],
  account_error: null, usage_error: null,
}];
let reads = 0;
globalThis.fetch = async (input) => {
  if (String(input) !== "/api/platform-account") throw new Error("Unexpected request");
  const next = responses[reads++];
  if (next === "fail") return new Response("", { status: 503 });
  return new Response(JSON.stringify(next), { status: 200 });
};
function assert(value: unknown, message: string): asserts value {
  if (!value) throw new Error(message);
}
const host = document.createElement("div");
document.body.append(host);
const root = createRoot(host);
const flush = async () => {
  await React.act(async () => { await new Promise((resolve) => setTimeout(resolve, 0)); });
};
const refresh = async () => {
  const button = host.querySelector<HTMLButtonElement>(".account-actions button");
  assert(button, "refresh remains available");
  await React.act(async () => button.click());
  await flush();
};
(async () => {
  await React.act(async () => root.render(React.createElement(Account)));
  await flush();
  assert(host.textContent?.includes("123.4"), "saved balance appears");
  assert(host.querySelector(".account-identity strong")?.textContent === "Acme", "account identity is visible");
  assert(host.textContent?.includes("Agent work"), "usage appears");
  assert(host.querySelector<HTMLAnchorElement>(".account-actions a")?.target === "_blank", "safe manage link opens separately");
  assert(host.textContent?.includes("Member information is not available"), "member limitation is explicit");

  await refresh();
  assert(host.textContent?.includes("Couldn’t refresh account"), "failed refresh is explicit");
  assert(host.textContent?.includes("123.4"), "saved data remains readable after failed refresh");
  assert(host.textContent?.includes("last successful check"), "saved data is labeled as stale");

  await refresh();
  assert(host.textContent?.includes("Balance and plan unavailable"), "partial account error is explicit");
  assert(host.textContent?.includes("1.2 HKD"), "usage keeps its currency when account data is absent");
  assert(host.querySelector(".account-members-note"), "member limitation remains visible on partial failure");

  await refresh();
  assert(host.textContent?.includes("Usage unavailable"), "partial usage error is explicit");

  await refresh();
  assert(host.textContent?.includes("No connected platform account"), "unconfigured state is explained");
  assert(host.querySelector(".account-actions button"), "unconfigured state can refresh");
  assert(!host.querySelector(".account-actions a"), "unconfigured state has no manage link");
  assert(reads === 5, "each refresh made one read");
  await React.act(async () => root.unmount());
})().catch((error) => { throw error; });
