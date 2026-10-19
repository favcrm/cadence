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
type Snapshot = import("../src/lib/types").AgentSettingsSnapshot;
type Config = import("../src/lib/types").AgentSettingsConfig;

const baseConfig = (): Config => ({
  harnesses: [{ id: "pi", label: "Pi", cli: "pi", note: "start" }],
  providers: [{ id: "codex", label: "Codex", kind: "native CLI", pin: "", gate: false, paid: false, note: "" }],
  models: [{ id: "gpt", provider: "codex", ctx: "", out: "", thinking: null, images: null }],
  profiles: [{ id: "developer", role: "dev", model: "inherit", job: "one ticket" }],
  scope: {},
  roleDefaults: { pm: "", research: "", architect: "", dev: "gpt", qa: "", devops: "", worker: "" },
  pins: [],
});

let snapshot: Snapshot = {
  revision: "4",
  read_only: false,
  config: baseConfig(),
  observations: { harnesses: {}, models: {}, profiles: {} },
  catalogs: {},
};
let failRead = false;
let holdRead = false;
let resolveRead: ((value: Response) => void) | undefined;
let failSave: "error" | "conflict" | null = null;
const saves: { expected_revision: string; config: Config }[] = [];
const json = (body: unknown, status = 200) =>
  new Response(JSON.stringify(body), { status, headers: { "Content-Type": "application/json" } });
globalThis.fetch = async (input, init) => {
  const url = String(input);
  if (!url.includes("/api/settings/agents")) throw new Error(`Unexpected request: ${url}`);
  if (init?.method === "POST") {
    const body = JSON.parse(String(init.body));
    saves.push(body);
    if (failSave === "error") return json({ error: "Cannot save right now" }, 500);
    if (failSave === "conflict") return json({ error: "Revision changed", code: "revision_conflict" }, 409);
    snapshot = { ...snapshot, revision: String(Number(snapshot.revision) + 1), config: body.config };
    return json(snapshot);
  }
  if (holdRead) {
    return new Promise<Response>((resolve) => {
      resolveRead = resolve;
    });
  }
  return failRead ? json({ error: "No response from daemon" }, 503) : json(snapshot);
};
win.HTMLElement.prototype.scrollIntoView = () => {};
const host = document.createElement("div");
document.body.append(host);
let root = createRoot(host);
const assert: (value: unknown, message: string) => asserts value = (value, message) => {
  if (!value) throw new Error(message);
};
const flush = () => React.act(async () => { await new Promise((resolve) => setTimeout(resolve, 0)); });
const render = async (gate: string | null = null) => {
  await React.act(async () => root.render(React.createElement(WriteGate.Provider, { value: gate }, React.createElement(ModelDefaults))));
  await flush();
};
const button = (label: string) =>
  Array.from(host.querySelectorAll<HTMLButtonElement>("button")).find((el) => {
    const text = el.textContent?.replace(/\s+/g, " ").trim() ?? "";
    return text === label || text.startsWith(label);
  });
const click = async (el: Element | null | undefined) => {
  assert(el, "Click target exists");
  await React.act(async () => el.dispatchEvent(new MouseEvent("click", { bubbles: true, button: 0 })));
  await flush();
};
const harnessNote = () => host.querySelector(".hnote")?.textContent;
const setHarnessNote = async (value: string) => {
  await click(button("Harnesses"));
  await click(host.querySelector(".hrow"));
  const input = host.querySelector<HTMLTextAreaElement>("[role=dialog] textarea");
  assert(input, "Harness note editor exists");
  const setter = Object.getOwnPropertyDescriptor(win.HTMLTextAreaElement.prototype, "value")!.set!;
  await React.act(async () => {
    setter.call(input, value);
    input.dispatchEvent(new Event("input", { bubbles: true }));
  });
  await flush();
  await click(button("Close"));
};
const fresh = async (gate: string | null = null) => {
  await React.act(async () => root.unmount());
  root = createRoot(host);
  await render(gate);
};
const openModels = async () => {
  await click(button("Providers & models"));
};

async function main() {
  holdRead = true;
  await render();
  assert(host.querySelector("[aria-busy=true]") && !host.querySelector("[role=tablist]"), "Pending read stays busy and does not invent an empty workspace");
  holdRead = false;
  await React.act(async () => resolveRead!(json({ error: "No response from daemon" }, 503)));
  await flush();
  assert(host.querySelector("[role=alert]") && button("Retry") && !host.querySelector("[role=tablist]"), "Read failure offers retry instead of an empty editor");
  failRead = false;
  await click(button("Retry"));
  assert(host.querySelector("[role=tablist]") && button("Save")?.disabled, "Loaded saved revision is not dirty");

  await setHarnessNote("saved note");
  assert(!button("Save")?.disabled, "Harness edit dirties the whole draft");
  await click(button("Save"));
  assert(saves[0].expected_revision === "4" && saves[0].config.harnesses[0].note === "saved note" && saves[0].config.roleDefaults.dev === "gpt", "Save posts the string revision and retains stored inherited defaults");
  assert(button("Save")?.disabled && harnessNote() === "saved note", "Successful save adopts the returned snapshot");

  await setHarnessNote("unsaved note");
  failSave = "error";
  await click(button("Save"));
  assert(!button("Save")?.disabled && harnessNote() === "unsaved note", "Failed save keeps the draft editable");
  failSave = "conflict";
  await click(button("Save"));
  assert(button("Save")?.disabled && button("Reload server copy") && harnessNote() === "unsaved note", "Conflict keeps the draft and blocks another write");
  failSave = null;
  await click(button("Discard"));
  assert(!button("Reload server copy") && button("Save")?.disabled && harnessNote() === "saved note", "Discard restores the saved revision and clears the conflict");

  await setHarnessNote("unsaved note");
  failSave = "conflict";
  await click(button("Save"));
  failSave = null;
  const serverConfig = baseConfig();
  serverConfig.harnesses[0].note = "server note";
  snapshot = { ...snapshot, revision: "9", config: serverConfig };
  holdRead = true;
  await click(button("Reload server copy"));
  assert(button("Save")?.disabled && button("Add harness")?.disabled, "Reload freezes editing while the server copy replaces the draft");
  holdRead = false;
  await React.act(async () => resolveRead!(json(snapshot)));
  await flush();
  assert(button("Save")?.disabled && !button("Reload server copy") && harnessNote() === "server note", "Reload adopts the server revision and drops the draft");

  await openModels();
  await click(button("Add pin"));
  assert(button("Save")?.disabled && host.querySelectorAll(".pinrow").length === 1, "Incomplete pin blocks save");
  await click(button("Discard"));
  assert(host.querySelectorAll(".pinrow").length === 0 && button("Save")?.disabled, "Discard drops the unsaved pin");

  await fresh("Sign in to act as operator");
  await openModels();
  assert(button("Add provider")?.disabled && button("Save")?.disabled, "Operator gate freezes editing");
  const count = saves.length;
  await click(button("Save"));
  assert(saves.length === count, "Read-only click makes no write");
  snapshot = { ...snapshot, read_only: true };
  await fresh();
  await openModels();
  assert(button("Add provider")?.disabled && button("Save")?.disabled, "Server read-only snapshot keeps editing blocked");

  snapshot = { ...snapshot, read_only: false, config: { harnesses: [], providers: [], models: [], profiles: [], scope: {}, roleDefaults: {}, pins: [] }, revision: "10" };
  await fresh();
  assert(host.querySelector(".emptyrow") && !host.querySelector("[role=alert]") && button("Save")?.disabled, "An empty saved document is an empty workspace, not a load error");
  await React.act(async () => root.unmount());
  console.log("agents settings draft checks passed");
}
main().catch((err) => { throw err; });
