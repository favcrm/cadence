import type { Meta } from "../src/lib/types";

declare function require(name: string): any;
const { Window } = require("happy-dom");
const win = new Window({ url: "http://localhost/" });
for (const key of ["window", "document", "navigator", "HTMLElement", "Node", "Event", "KeyboardEvent", "location", "localStorage"]) {
  Object.defineProperty(globalThis, key, { value: key === "window" ? win : win[key], configurable: true });
}
Object.defineProperty(globalThis, "IS_REACT_ACT_ENVIRONMENT", { value: true });
const calls: string[] = [];
Object.defineProperty(globalThis, "fetch", {
  configurable: true,
  value: async (url: string) => {
    calls.push(String(url));
    return new Response("{}", { status: 200, headers: { "Content-Type": "application/json" } });
  },
});
const moduleLoader = require("module");
const originalRequire = moduleLoader.prototype.require;
moduleLoader.prototype.require = function (this: unknown, id: string) {
  if (id === "@hugeicons/core-free-icons") return new Proxy({}, { get: () => ({}) });
  if (id === "@hugeicons/react") return { HugeiconsIcon: () => null };
  return originalRequire.apply(this, arguments);
};
const { createElement, act } = require("react");
const { createRoot } = require("react-dom/client");
const { default: AccountMenu } = require("../src/ui/AccountMenu") as typeof import("../src/ui/AccountMenu");
const { default: SignIn } = require("../src/features/auth/SignIn") as typeof import("../src/features/auth/SignIn");

function assert(condition: unknown, message: string): void {
  if (!condition) throw new Error(message);
}

const user = { name: "Fable Chen", email: "fable@example.com", role: "operator", handle: "fable", sub: "s" };
const signedIn = { read_only: false, signed_in: true, actor: "Fable (board)", session: { id: "sess-1", origin: "public", user } } as unknown as Meta;
const signedOut = { read_only: false, signed_in: false, actor: "x" } as unknown as Meta;

function mount(node: unknown) {
  const host = document.createElement("div");
  document.body.append(host);
  const root = createRoot(host);
  act(() => root.render(node));
  return { host, done: () => { act(() => root.unmount()); host.remove(); } };
}
const menu = (meta: Meta | null, extra: object = {}, onChange: () => void = () => undefined) =>
  createElement(AccountMenu, { meta, actor: "Fable (board)", mayWrite: true, onChange, trigger: "avatar", placement: "below-end", ...extra });
const trigger = (host: any) => host.querySelector("button[aria-haspopup=menu]") as HTMLButtonElement;
const panel = (host: any) => host.querySelector("[role=menu]");
const pointerDown = (target: any) => act(() => { target.dispatchEvent(new win.Event("pointerdown", { bubbles: true })); });

// Open and close: click, Escape, outside click; focus returns to the trigger.
{
  const { host, done } = mount(menu(signedIn));
  const t = trigger(host);
  assert(t.getAttribute("aria-expanded") === "false" && !panel(host), "closed at first");
  assert(t.textContent?.trim() === "F", "avatar shows the initial");
  act(() => t.click());
  assert(t.getAttribute("aria-expanded") === "true" && panel(host), "click opens");
  act(() => t.click());
  assert(!panel(host), "click again closes");
  act(() => t.click());
  const first = panel(host).querySelector("button") as HTMLButtonElement;
  first.focus();
  act(() => { first.dispatchEvent(new win.KeyboardEvent("keydown", { key: "Escape", bubbles: true })); });
  assert(!panel(host), "Escape closes");
  assert(document.activeElement === t, "Escape returns focus to the trigger");
  act(() => t.click());
  pointerDown(document.body);
  assert(!panel(host), "outside click closes");
  act(() => t.click());
  pointerDown(panel(host));
  assert(panel(host), "a pointer inside the panel keeps it open");
  done();
}

// Identity and the writes line.
{
  const { host, done } = mount(menu(signedIn));
  act(() => trigger(host).click());
  const text = panel(host).textContent as string;
  assert(text.includes("Fable Chen") && text.includes("fable@example.com") && text.includes("operator"), "identity block");
  assert(text.includes("Writes commit to the tracker as") && text.includes("Fable (board)"), "writes line when the client may write");
  assert(host.querySelector("[title='session sess-1']"), "session id kept in a title");
  done();
}
{
  const { host, done } = mount(menu(signedIn, { mayWrite: false }));
  act(() => trigger(host).click());
  assert(!(panel(host).textContent as string).includes("Writes commit"), "no writes line on a read-only client");
  done();
}

// Theme selection calls setThemePref (stored + applied).
{
  const { host, done } = mount(menu(signedIn));
  act(() => trigger(host).click());
  const buttons = Array.from(panel(host).querySelectorAll("[aria-pressed]")) as HTMLButtonElement[];
  assert(buttons.length === 3 && buttons[0].getAttribute("aria-pressed") === "true", "System pressed by default");
  const dark = buttons.find((b) => b.textContent === "Dark")!;
  act(() => dark.click());
  assert(dark.getAttribute("aria-pressed") === "true", "Dark pressed");
  assert(document.documentElement.getAttribute("data-theme") === "dark", "data-theme applied");
  assert(win.localStorage.getItem("cadence-theme") === "dark", "pick stored");
  act(() => buttons[0].click());
  assert(!document.documentElement.hasAttribute("data-theme"), "System clears data-theme");
  done();
}

// Sign out: closeSession, then onChange.
async function signOutCase() {
  let changed = 0;
  const { host, done } = mount(menu(signedIn, {}, () => { changed += 1; }));
  act(() => trigger(host).click());
  const out = Array.from(panel(host).querySelectorAll("button")).find((b: any) => b.textContent === "Sign out") as HTMLButtonElement;
  await act(async () => { out.click(); });
  assert(calls.includes("/api/session/logout"), "sign out posts the logout");
  assert(changed === 1, "onChange runs once after sign out");
  done();
}

async function guardCases() {
  const ro = { ...signedIn, read_only: true } as unknown as Meta;
  {
    const { host, done } = mount(menu(ro));
    act(() => trigger(host).click());
    const text = panel(host).textContent as string;
    assert(!text.includes("Sign out") && !text.includes("Writes commit"), "read-only board: no sign out, no writes line");
    assert(text.includes("Fable Chen") && host.querySelector("[aria-pressed]"), "read-only board keeps identity and theme");
    done();
  }
  {
    calls.length = 0;
    const { host, done } = mount(menu(signedIn));
    act(() => trigger(host).click());
    const out = Array.from(panel(host).querySelectorAll("button")).find((b: any) => b.textContent === "Sign out") as HTMLButtonElement;
    await act(async () => { out.click(); out.click(); });
    assert(calls.filter((c) => c === "/api/session/logout").length === 1, "double click signs out once");
    done();
  }
  {
    const realFetch = globalThis.fetch;
    Object.defineProperty(globalThis, "fetch", { configurable: true, value: async () => { throw new Error("down"); } });
    let changed = 0;
    const { host, done } = mount(menu(signedIn, {}, () => { changed += 1; }));
    act(() => trigger(host).click());
    const out = Array.from(panel(host).querySelectorAll("button")).find((b: any) => b.textContent === "Sign out") as HTMLButtonElement;
    await act(async () => { out.click(); });
    assert(changed === 1, "closeSession rejects: onChange still runs once");
    Object.defineProperty(globalThis, "fetch", { configurable: true, value: realFetch });
    done();
  }
}
function rest() {
// Signed out: no account menu, the SignIn disclosure stays.
{
  const { host, done } = mount(createElement("div", null, menu(signedOut), createElement(SignIn, { meta: signedOut, onChange: () => undefined })));
  assert(!host.querySelector("[data-account-menu]"), "no account menu when signed out");
  assert(host.querySelector("details summary")?.textContent?.includes("Sign in"), "SignIn disclosure present");
  done();
}
{
  const { host, done } = mount(createElement("div", null, menu(signedIn), createElement(SignIn, { meta: signedIn, onChange: () => undefined })));
  assert(host.querySelector("[data-account-menu]") && !host.querySelector("details"), "signed in: menu, no SignIn chip");
  done();
}
}
signOutCase().then(guardCases).then(() => { rest(); console.log("account menu checks passed"); }, (e) => { console.error(e); require("process").exit(1); });
