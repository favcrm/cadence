export {};
/** Publication navigation/lifetime and preserved operator proof; Markdown itself is covered in browser verification. */
declare function require(name: string): any;
const { Window } = require("happy-dom");
const win = new Window({ url: "http://localhost/outbox" });
for (const name of [
  "window",
  "document",
  "Node",
  "Element",
  "HTMLElement",
  "HTMLInputElement",
  "SVGElement",
  "navigator",
  "MutationObserver",
  "Event",
  "MouseEvent",
  "KeyboardEvent",
  "location",
  "history",
]) {
  Object.defineProperty(globalThis, name, {
    value: name === "window" ? win : win[name],
    configurable: true,
    writable: true,
  });
}
for (const name of ["addEventListener", "removeEventListener"])
  Object.defineProperty(globalThis, name, {
    value: win[name].bind(win),
    configurable: true,
  });
Object.defineProperty(globalThis, "IS_REACT_ACT_ENVIRONMENT", { value: true });
const loader = require("module");
const originalRequire = loader.prototype.require;
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
const ModelDefaults = (
  require("../src/features/settings/ModelDefaults") as typeof import("../src/features/settings/ModelDefaults")
).default;
const { WriteGate } =
  require("../src/features/auth/WriteGate") as typeof import("../src/features/auth/WriteGate");
type Snapshot = import("../src/lib/types").ModelDefaultsSnapshot;
type Config = import("../src/lib/types").ModelDefaultsConfig;
const providers = [
  {
    id: "codex",
    label: "Codex",
    eligible: true,
    kinds: ["managed"],
    suggestions: ["known-model"],
    suggestions_note: "Previously observed, not a catalog.",
  },
  {
    id: "pi",
    label: "Pi",
    eligible: true,
    kinds: ["managed"],
    suggestions: [],
    suggestions_note: "No models observed.",
  },
  {
    id: "devin",
    label: "Devin",
    eligible: false,
    kinds: [],
    suggestions: [],
    suggestions_note: "",
    limitation: "Selection belongs to this provider.",
  },
];
let snapshot: Snapshot = {
  revision: 4,
  read_only: false,
  config: { schema: 1, providers: {} },
  providers,
  roles: [{ id: "developer", label: "Developer" }],
};
let failRead = false;
let holdRead = false;
let resolveRead: ((value: Response) => void) | undefined;
let failSave: "error" | "conflict" | null = null;
const saves: { expected_revision: number; config: Config }[] = [];
const json = (body: unknown, status = 200) =>
  new Response(JSON.stringify(body), {
    status,
    headers: { "Content-Type": "application/json" },
  });
globalThis.fetch = async (input, init) => {
  if (!String(input).includes("/api/settings/model-defaults"))
    throw new Error(`Unexpected request: ${input}`);
  if (init?.method === "POST") {
    const body = JSON.parse(String(init.body));
    saves.push(body);
    if (failSave === "error")
      return json({ error: "Cannot save right now" }, 500);
    if (failSave === "conflict")
      return json(
        { error: "Revision changed", code: "revision_conflict", revision: 9 },
        409,
      );
    snapshot = {
      ...snapshot,
      revision: snapshot.revision + 1,
      config: body.config,
    };
    return json(snapshot);
  }
  if (holdRead)
    return new Promise<Response>((resolve) => {
      resolveRead = resolve;
    });
  return failRead
    ? json({ error: "No response from daemon" }, 503)
    : json(snapshot);
};
win.HTMLElement.prototype.scrollIntoView = () => {};
const host = document.createElement("div");
document.body.append(host);
let root = createRoot(host);
const assert: (value: unknown, message: string) => asserts value = (
  value,
  message,
) => {
  if (!value) throw new Error(message);
};
const flush = () =>
  React.act(async () => {
    await new Promise((resolve) => setTimeout(resolve, 0));
  });
const render = async (gate: string | null = null) => {
  await React.act(async () =>
    root.render(
      React.createElement(
        WriteGate.Provider,
        { value: gate },
        React.createElement(ModelDefaults),
      ),
    ),
  );
  await flush();
};
const button = (label: string) =>
  Array.from(host.querySelectorAll<HTMLButtonElement>("button")).find(
    (el) => el.textContent?.trim() === label,
  );
const click = async (el: Element | null | undefined) => {
  assert(el, "Click target exists");
  await React.act(async () =>
    el.dispatchEvent(new MouseEvent("click", { bubbles: true, button: 0 })),
  );
  await flush();
};
const choose = async (id: string, value: string) => {
  await click(document.getElementById(id));
  const option = Array.from(document.querySelectorAll('[role="option"]')).find(
    (el) => el.textContent?.trim() === value,
  );
  await click(option);
};
const fill = async (id: string, value: string) => {
  const input = document.getElementById(id);
  assert(input, "Model ID input exists");
  const setter = Object.getOwnPropertyDescriptor(
    win.HTMLInputElement.prototype,
    "value",
  )!.set!;
  await React.act(async () => {
    setter.call(input, value);
    input.dispatchEvent(new Event("input", { bubbles: true }));
  });
  await flush();
};
const fresh = async (gate: string | null = null) => {
  await React.act(async () => root.unmount());
  root = createRoot(host);
  await render(gate);
};

async function main() {
  holdRead = true;
  await render();
  assert(
    host.textContent?.includes("Loading model defaults") &&
      !host.textContent?.includes("No model providers"),
    "Pending read is loading rather than a false empty state",
  );
  holdRead = false;
  await React.act(async () =>
    resolveRead!(json({ error: "No response from daemon" }, 503)),
  );
  await flush();
  assert(
    host.textContent?.includes("not reachable") && button("Retry"),
    "Initial read failure offers retry rather than empty providers",
  );
  failRead = false;
  await click(button("Retry"));
  assert(
    host.querySelectorAll("article").length === 1 &&
      host.querySelector("article")?.textContent?.includes("Codex"),
    "One provider editor is visible after retry",
  );
  assert(button("Save defaults")?.disabled, "Untouched config is not dirty");
  await choose("codex-baseline", "Specific model");
  await fill("codex-baseline-model", "codex-custom");
  await click(button("Pi"));
  assert(
    host.textContent?.includes("Operator policy default") &&
      host.textContent?.includes("Pi policy") &&
      !host.textContent?.includes("lets the provider choose"),
    "Pi fallback is explained as operator policy, not provider choice",
  );
  await choose("pi-baseline", "Specific model");
  await choose("pi-baseline", "Operator policy default");
  await choose("pi-developer", "Operator policy default");
  await choose("pi-developer", "Specific model");
  await fill("pi-developer-model", "pi-custom");
  await click(button("CodexUnsaved"));
  assert(
    (document.getElementById("codex-baseline-model") as HTMLInputElement)
      .value === "codex-custom",
    "Switching preserves first provider draft",
  );
  assert(
    host.textContent?.includes("2 providers"),
    "Save status includes changes across providers",
  );
  await click(button("Save defaults"));
  assert(saves[0].expected_revision === 4, "Save retains optimistic revision");
  assert(
    saves[0].config.providers.codex.default.model === "codex-custom" &&
      saves[0].config.providers.pi.default.mode === "provider_default" &&
      saves[0].config.providers.pi.roles.developer.model === "pi-custom",
    "Save applies both provider drafts with role override",
  );
  assert(
    button("Save defaults")?.disabled &&
      host.textContent?.includes("Model defaults saved"),
    "Successful save adopts snapshot and reports saved",
  );

  await choose("codex-developer", "Provider-native default");
  failSave = "error";
  await click(button("Save defaults"));
  assert(
    host.textContent?.includes("Cannot save right now") &&
      !button("Save defaults")?.disabled,
    "Failed save retains an actionable draft",
  );
  failSave = "conflict";
  await click(button("Save defaults"));
  assert(
    host.textContent?.includes("Current server revision: 9") &&
      button("Reload server copy"),
    "Conflict retains draft and offers explicit replacement",
  );
  assert(
    host.textContent?.includes("Unsaved changes"),
    "Conflict does not discard unsaved work",
  );
  failSave = null;
  await click(button("Discard changes"));
  assert(
    !host.textContent?.includes("Current server revision") &&
      button("Save defaults")?.disabled,
    "Discard restores all saved changes and clears conflict",
  );

  await choose("codex-developer", "Provider-native default");
  failSave = "conflict";
  await click(button("Save defaults"));
  failSave = null;
  snapshot = { ...snapshot, revision: 9 };
  holdRead = true;
  await click(button("Reload server copy"));
  assert(
    button("Save defaults")?.disabled &&
      (document.getElementById("codex-baseline") as HTMLButtonElement).disabled,
    "Reload freezes editing while replacing the server snapshot",
  );
  holdRead = false;
  await React.act(async () => resolveRead!(json(snapshot)));
  await flush();
  assert(
    host.textContent?.includes("Saved revision 9") &&
      button("Save defaults")?.disabled &&
      !host.textContent?.includes("Current server revision"),
    "Explicit conflict reload adopts server revision and clears the draft",
  );

  await click(button("Reset Codex defaults"));
  await click(button("Save defaults"));
  const last = saves[saves.length - 1];
  assert(
    !last.config.providers.codex &&
      last.config.providers.pi.roles.developer.model === "pi-custom",
    "Reset omits selected provider and retains another provider",
  );
  await click(button("DevinUnavailable"));
  assert(
    host.textContent?.includes("Selection belongs to this provider") &&
      !host.querySelector("article [role=combobox]"),
    "Unsupported provider is inspectable with limitation and no editable selectors",
  );

  await fresh("Sign in to act as operator");
  assert(
    host.textContent?.includes("Sign in using the top bar"),
    "Signed-out state explains sign in",
  );
  assert(
    Array.from(
      host.querySelectorAll<HTMLButtonElement>("article button"),
    ).every((el) => el.disabled),
    "Gate freezes model selectors and reset",
  );
  const count = saves.length;
  await click(button("Save defaults"));
  assert(saves.length === count, "Read-only click makes no write");
  snapshot = { ...snapshot, read_only: true };
  await fresh();
  assert(
    host.textContent?.includes("This board is read-only"),
    "Server read-only snapshot keeps editing blocked",
  );

  snapshot = { ...snapshot, read_only: false };
  await fresh();
  await choose("codex-baseline", "Specific model");
  assert(
    button("Save defaults")?.disabled && host.textContent?.includes("model"),
    "Empty explicit model blocks save",
  );
  await fill("codex-baseline-model", "x".repeat(201));
  assert(
    button("Save defaults")?.disabled &&
      host.textContent?.includes("at most 200 bytes") &&
      host.querySelector('[aria-label="Model validation errors"]'),
    "Invalid model shows validation and remains blocked",
  );
  await fill("codex-baseline-model", "valid-model");
  assert(!button("Save defaults")?.disabled, "Correction allows a valid save");
  await click(button("Pi"));
  await choose("pi-baseline", "Specific model");
  await fill("pi-baseline-model", "other-model");
  await click(button("Discard changes"));
  await click(button("Codex"));
  assert(
    !document.getElementById("codex-baseline-model") &&
      button("Save defaults")?.disabled,
    "Discard clears all provider drafts, including hidden ones",
  );

  snapshot = { ...snapshot, providers: [] };
  await fresh();
  assert(
    host.textContent?.includes("No model providers available") &&
      button("Refresh providers"),
    "Resolved empty provider list is distinct and recoverable",
  );
  await React.act(async () => root.unmount());
  console.log("model defaults provider workspace checks passed");
}
main().catch((err) => {
  throw err;
});
