export {};
/** Saved permission reads/actions exercise the real component and shared Button. */
declare function require(name: string): any;
const { Window } = require("happy-dom");
const win = new Window({ url: "http://localhost/settings/permissions" });
for (const name of [
  "window",
  "document",
  "Node",
  "Element",
  "HTMLElement",
  "SVGElement",
  "navigator",
  "MutationObserver",
  "Event",
  "MouseEvent",
  "location",
  "history",
])
  Object.defineProperty(globalThis, name, {
    value: name === "window" ? win : win[name],
    configurable: true,
    writable: true,
  });
for (const name of ["addEventListener", "removeEventListener"])
  Object.defineProperty(globalThis, name, {
    value: win[name].bind(win),
    configurable: true,
  });
Object.defineProperty(globalThis, "IS_REACT_ACT_ENVIRONMENT", { value: true });
const loader = require("module"),
  originalRequire = loader.prototype.require;
loader.prototype.require = function (this: unknown, id: string) {
  if (id.endsWith(".css")) return {};
  if (id === "@hugeicons/core-free-icons")
    return new Proxy({}, { get: () => ({}) });
  if (id === "@hugeicons/react") return { HugeiconsIcon: () => null };
  return originalRequire.apply(this, arguments);
};
const React = require("react") as typeof import("react");
const { createRoot } =
  require("react-dom/client") as typeof import("react-dom/client");
const Permissions = (
  require("../src/features/settings/MasterPermissions") as typeof import("../src/features/settings/MasterPermissions")
).default;

type Rule = import("../src/lib/api").MasterPermissionRule;
const sample = (): Rule[] => [
  {
    id: "allow/a",
    effect: "allow",
    scope: "exact",
    argv: ["git", "log", "--format=two words"],
    tail: [],
    cwd: "/workspace/site",
    by: "operator",
    at: 1700000000,
  },
  {
    id: "deny/b",
    effect: "deny",
    scope: "prefix",
    argv: ["cat"],
    tail: ["/workspace/private/*"],
    cwd: "/workspace/site",
    by: "reviewer",
    at: 1700000001,
  },
];
const json = (data: unknown, code = 200) =>
  new Response(JSON.stringify(data), {
    status: code,
    headers: { "Content-Type": "application/json" },
  });
let rules = sample(),
  fail = false,
  hold = false;
const gets: { resolve: (r: Response) => void }[] = [],
  posts: { path: string; body: string; resolve: (r: Response) => void }[] = [];
let reads = 0;
globalThis.fetch = async (input, init) => {
  const path = new URL(String(input), "http://localhost").pathname;
  if (init?.method === "POST")
    return new Promise<Response>((resolve) =>
      posts.push({ path, body: String(init.body), resolve }),
    );
  if (path !== "/api/master/permissions")
    throw new Error(`Unexpected read ${path}`);
  reads++;
  if (hold) return new Promise<Response>((resolve) => gets.push({ resolve }));
  return fail
    ? json({ error: "Rules temporarily unavailable" }, 503)
    : json({ rules, requests: [] });
};
const host = document.createElement("div");
document.body.append(host);
let root = createRoot(host);
const assert: (v: unknown, why: string) => asserts v = (v, why) => {
  if (!v) throw new Error(why);
};
const flush = () =>
  React.act(async () => {
    await new Promise((r) => setTimeout(r, 0));
  });
const text = () => host.textContent ?? "";
const button = (label: string) =>
  Array.from(host.querySelectorAll("button")).find(
    (b) => b.textContent?.trim() === label,
  );
const click = async (el: Element | undefined) => {
  assert(el, "Click target exists");
  await React.act(async () =>
    el.dispatchEvent(new MouseEvent("click", { bubbles: true })),
  );
  await flush();
};
const render = async (readOnly = false, operator = true) => {
  // The original component has no viewer input; keeping the baseline compilable proves its missing guard.
  await React.act(async () =>
    root.render(
      React.createElement(Permissions as React.ComponentType<any>, {
        viewer: { readOnly, operator },
      }),
    ),
  );
  await flush();
};
const fresh = async (readOnly = false, operator = true) => {
  await React.act(async () => root.unmount());
  root = createRoot(host);
  rules = sample();
  fail = hold = false;
  reads = 0;
  gets.length = posts.length = 0;
  await render(readOnly, operator);
};
const revoke = (id: string) =>
  host.querySelector(`[data-rule="${id}"] button`) as HTMLButtonElement;
const fill = async (value: string) => {
  const input = host.querySelector('input[type="search"]');
  await React.act(async () => {
    Object.getOwnPropertyDescriptor(
      win.HTMLInputElement.prototype,
      "value",
    )!.set!.call(input, value);
    input!.dispatchEvent(new Event("input", { bubbles: true }));
  });
  await flush();
};
const choose = async (label: string, option: string) => {
  const control = Array.from(host.querySelectorAll("label")).find(
    (el) => el.textContent === label,
  )!;
  await click(document.getElementById(control.htmlFor) ?? undefined);
  await click(
    Array.from(document.querySelectorAll('[role="option"]')).find(
      (el) => el.textContent?.trim() === option,
    ),
  );
};
async function main() {
  const only = (globalThis as any).process.env.PERMISSION_CASE;
  if (only === "gate") {
    await render(false, false);
    assert(reads === 0, "An unproven viewer must not read protected rules");
  } else if (only === "failure") {
    fail = true;
    await render();
    assert(!text().includes("Loading"), "Initial failure stops loading");
    assert(button("Retry rules"), "Failed rules can retry");
  } else if (only === "readonly") {
    await render(true);
    await click(revoke("allow/a"));
    assert(posts.length === 0, "Read-only viewer must not revoke");
  } else {
    await fresh(false, false);
    assert(reads === 0, "Unproven viewer makes no protected GET");
    assert(text().includes("operator"), "Operator access is explained");
    await fresh(true);
    assert(revoke("allow/a").disabled, "Read-only operator cannot revoke");
    await click(revoke("allow/a"));
    assert(posts.length === 0, "Read-only issues no POST");
    await click(button("Refresh rules"));
    assert(Number(reads) === 2, "Read-only operator can refresh");
    await fresh();
    assert(
      text().includes("/workspace/site") &&
        text().includes('"--format=two words"'),
      "Complete cwd and quoted argument boundaries are visible",
    );
    assert(
      text().includes("Argument pattern") && text().includes("Exact command"),
      "Scope labels distinguish exact and argument patterns",
    );
    await fill("reviewer");
    assert(
      host.querySelectorAll("[data-rule]").length === 1 &&
        host.querySelector('[data-rule="deny/b"]'),
      "Search includes saved author",
    );
    await click(button("Clear filters"));
    await choose("Effect", "Deny");
    await choose("Match", "Exact command");
    assert(
      text().includes("No matching rules"),
      "Combined filters distinguish no matches",
    );
    await click(button("Show all rules"));
    assert(
      host.querySelectorAll("[data-rule]").length === 2,
      "Clear restores all rules",
    );
    await fill("missing");
    fail = true;
    await click(button("Refresh rules"));
    assert(
      text().includes("No last known matches") &&
        !text().includes("No saved rules"),
      "Failed filtered read cannot claim a valid empty store",
    );
    fail = false;
    await click(button("Retry rules"));
    await click(button("Clear filters"));
    await React.act(async () => root.unmount());
    root = createRoot(host);
    fail = true;
    await render();
    assert(
      text().includes("Rules unavailable") && !text().includes("Loading"),
      "Initial failure is distinct from loading and empty",
    );
    fail = false;
    await click(button("Retry rules"));
    assert(revoke("allow/a"), "Retry restores rules");
    fail = true;
    await click(button("Refresh rules"));
    assert(
      text().includes("Last known") && revoke("allow/a").disabled,
      "Stale rows are retained and not writable",
    );
    fail = false;
    await click(button("Retry rules"));
    hold = true;
    await click(button("Refresh rules"));
    await click(button("Refresh rules"));
    gets[1].resolve(
      json({
        rules: [{ ...sample()[0], by: "new observation" }],
        requests: [],
      }),
    );
    await flush();
    gets[0].resolve(
      json({
        rules: [{ ...sample()[0], by: "old observation" }],
        requests: [],
      }),
    );
    await flush();
    assert(
      text().includes("new observation") && !text().includes("old observation"),
      "Overlapping old GET cannot paint",
    );
    await fresh();
    hold = true;
    await click(button("Refresh rules"));
    const lost = gets.at(-1)!;
    await render(false, false);
    assert(
      !text().includes("/workspace/site"),
      "Lost access hides saved rules immediately",
    );
    await render();
    const recovered = gets.at(-1)!;
    recovered.resolve(
      json({
        rules: [{ ...sample()[0], by: "recovered access" }],
        requests: [],
      }),
    );
    await flush();
    lost.resolve(
      json({ rules: [{ ...sample()[0], by: "revoked access" }], requests: [] }),
    );
    await flush();
    assert(
      text().includes("recovered access") && !text().includes("revoked access"),
      "Access loss and recovery invalidate the old lifetime",
    );
    await fresh();
    await click(revoke("deny/b"));
    assert(
      posts.length === 0 && text().includes("removes this restriction"),
      "Deny removal needs accurate confirmation",
    );
    assert(
      document.activeElement?.textContent?.trim() === "Cancel",
      "Confirmation focuses Cancel",
    );
    await click(button("Cancel"));
    assert(
      document.activeElement === revoke("deny/b"),
      "Cancel returns focus to the same rule",
    );
    await click(revoke("allow/a"));
    await render(true);
    assert(
      !button("Confirm revoke"),
      "Read-only transition clears confirmation",
    );
    await render();
    await click(revoke("allow/a"));
    const confirm = button("Confirm revoke")!;
    await React.act(async () => {
      confirm.dispatchEvent(new MouseEvent("click", { bubbles: true }));
      confirm.dispatchEvent(new MouseEvent("click", { bubbles: true }));
    });
    await flush();
    assert(
      Number(posts.length) === 1 &&
        posts[0].path === "/api/master/permissions/rules/allow%2Fa/revoke" &&
        posts[0].body === "{}",
      "Repeated confirm sends one exact ID with empty body",
    );
    assert(revoke("deny/b").disabled, "Other rules cannot revoke concurrently");
    posts[0].resolve(json({ error: "Operator session expired" }, 403));
    await flush();
    assert(
      text().includes("Couldn’t revoke") &&
        text().includes("Operator session expired") &&
        revoke("allow/a"),
      "Server rejection retains the rule and honest feedback",
    );
    assert(
      document.activeElement === button("Refresh rules"),
      "A rejected revoke returns keyboard focus to refresh",
    );
    await click(revoke("allow/a"));
    await click(button("Confirm revoke"));
    fail = true;
    posts[1].resolve(json(sample()[0]));
    await flush();
    assert(
      text().includes("Rule revoked") &&
        text().includes("Last known") &&
        !host.querySelector('[data-rule="allow/a"]'),
      "Confirmed revoke and failed follow-up GET are distinct; removed rule stays removed",
    );
    assert(
      revoke("deny/b").disabled,
      "Failed follow-up blocks remaining writes",
    );
    assert(
      document.activeElement === button("Retry rules"),
      "A committed revoke with failed refresh keeps keyboard focus on retry",
    );
    rules = [sample()[1]];
    fail = false;
    await click(button("Retry rules"));
    assert(
      !revoke("deny/b").disabled,
      "Fresh retry restores write affordances",
    );
    await fresh();
    await click(revoke("allow/a"));
    await click(button("Confirm revoke"));
    await render(false, false);
    await render();
    assert(
      !host.querySelector(".permission-revoke:not(:disabled)") &&
        text().includes("Waiting for revocation"),
      "In-flight POST remains locked across access loss and recovery",
    );
    rules = [sample()[1]];
    posts[0].resolve(json(sample()[0]));
    await flush();
    assert(
      !host.querySelector('[data-rule="allow/a"]') &&
        !revoke("deny/b").disabled,
      "New access refreshes after prior write settles",
    );
    hold = true;
    await click(button("Refresh rules"));
    const late = gets.at(-1)!;
    await React.act(async () => root.unmount());
    root = createRoot(host);
    hold = false;
    rules = [{ ...sample()[1], by: "fresh mount" }];
    await render();
    late.resolve(
      json({ rules: [{ ...sample()[1], by: "unmounted" }], requests: [] }),
    );
    await flush();
    assert(
      text().includes("fresh mount") && !text().includes("unmounted"),
      "Unmounted read cannot affect a new instance",
    );
  }
  await React.act(async () => root.unmount());
  console.log(`permissionsWorkspace ${only ?? "all"}: PASS`);
}
void main().catch((e: unknown) => {
  console.error(e);
  (globalThis as any).process.exitCode = 1;
});
