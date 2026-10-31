import type { Meta } from "../src/lib/types";
import type * as SessionKeyModule from "../src/lib/sessionKey";
import type PreferencesComponent from "../src/features/settings/Preferences";

declare function require(name: string): any;
const { Window } = require("happy-dom");
const win = new Window({ url: "http://localhost/" });
for (const key of ["window", "document", "navigator", "HTMLElement", "Node", "Event", "KeyboardEvent", "location", "localStorage", "sessionStorage"]) {
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
const { default: Preferences } = require("../src/features/settings/Preferences") as { default: typeof PreferencesComponent };
const { default: SignIn } = require("../src/features/auth/SignIn") as typeof import("../src/features/auth/SignIn");
const { LocaleProvider } = require("../src/lib/locale") as typeof import("../src/lib/locale");
const { sessionKey, setSessionKey } = require("../src/lib/sessionKey") as typeof SessionKeyModule;

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
  act(() => root.render(createElement(LocaleProvider, null, node)));
  return { host, done: () => { act(() => root.unmount()); host.remove(); } };
}
const menu = (meta: Meta | null, extra: object = {}, onChange: () => void = () => undefined) =>
  createElement(AccountMenu, { meta, onChange, trigger: "avatar", placement: "below-end", ...extra });
const trigger = (host: any) => host.querySelector("button[aria-haspopup=dialog]") as HTMLButtonElement;
const panel = (host: any) => host.querySelector("[role=dialog]");
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

// Tab past the last control closes the dialog; moving inside it, or to the trigger, does not.
{
  const { host, done } = mount(createElement("div", null, menu(signedIn), createElement("button", { id: "outside" }, "outside")));
  const t = trigger(host);
  const outside = host.querySelector("#outside") as HTMLButtonElement;
  act(() => t.click());
  const buttons = Array.from(panel(host).querySelectorAll("button")) as HTMLButtonElement[];
  act(() => buttons[0].focus());
  act(() => buttons[buttons.length - 1].focus());
  assert(panel(host), "focus moving between panel controls keeps it open");
  act(() => t.focus());
  assert(panel(host), "focus moving to the trigger keeps it open");
  act(() => buttons[buttons.length - 1].focus());
  act(() => outside.focus());
  assert(!panel(host), "Tab out of the panel closes the dialog");
  assert(document.activeElement === outside, "closing on focus-out does not steal focus back");
  done();
}

// Activating any link in the panel (here a footer slot item) closes the dialog; a plain button does not.
{
  const footer = createElement("div", null, createElement("a", { href: "#v", id: "footer-link" }, "v1"), createElement("button", { id: "footer-btn" }, "b"));
  const { host, done } = mount(menu(signedIn, { footer }));
  act(() => trigger(host).click());
  act(() => (host.querySelector("#footer-btn") as HTMLElement).click());
  assert(panel(host), "a non-link click in the panel keeps it open");
  act(() => (host.querySelector("#footer-link") as HTMLElement).click());
  assert(!panel(host), "clicking a link in the footer closes the dialog");
  done();
}

// The sidebar row trigger (CAD-1312): avatar, name and role on one compact
// line with a title and an up-chevron — no host, which the header owns now.
{
  const { host, done } = mount(menu(signedIn, { trigger: "row", placement: "above-start" }));
  const t = trigger(host);
  assert(t.className.includes("navlink") && t.className.includes("account-row"), "row trigger uses the navlink family");
  assert(t.textContent?.includes("Fable Chen") && t.textContent.includes("operator"), "row shows name and role");
  assert(!t.textContent?.includes("localhost"), "row no longer repeats the host");
  assert(host.querySelector("[title='Fable Chen · operator']"), "full name · role in a title");
  assert(t.querySelector(".truncate") && t.querySelector(".rotate-180"), "name truncates and the chevron points up");
  assert(!t.textContent?.includes("Sign out"), "row trigger is only the trigger");
  act(() => t.click());
  const cls = panel(host).className as string;
  assert(cls.includes("bottom-full") && cls.includes("left-0") && !cls.includes("top-full"), "above-start opens above, left-aligned");
  done();
}

// Identity remains visible in the account menu.
{
  const { host, done } = mount(menu(signedIn));
  act(() => trigger(host).click());
  const text = panel(host).textContent as string;
  assert(text.includes("Fable Chen") && text.includes("fable@example.com") && text.includes("operator"), "identity block");
  assert(host.querySelector("[title='session sess-1']"), "session id kept in a title");
  done();
}

// The settings preferences preserve theme selection and persistence.
{
  const { host, done } = mount(createElement(Preferences));
  const theme = host.querySelector('[role="combobox"][aria-label="Theme"]') as HTMLButtonElement;
  assert(theme?.textContent?.includes("System"), "System is selected by default");
  const key = (value: string) => act(() => theme.dispatchEvent(new win.KeyboardEvent("keydown", { key: value, bubbles: true })));
  key("Enter");
  key("End");
  key("Enter");
  assert(theme.textContent?.includes("Dark"), "Dark is selected");
  assert(document.documentElement.getAttribute("data-theme") === "dark", "data-theme applied");
  assert(win.localStorage.getItem("cadence-theme") === "dark", "pick stored");
  key("Enter");
  key("Home");
  key("Enter");
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
    assert(text.includes("Sign out"), "read-only boards keep session logout");
    assert(text.includes("Fable Chen"), "read-only board keeps identity");
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
    setSessionKey("retained-tab-key");
    let unavailable = 0;
    const failed = () => { unavailable += 1; };
    window.addEventListener("cadence:session-logout-unavailable", failed);
    const { host, done } = mount(menu(signedIn, {}, () => { changed += 1; }));
    act(() => trigger(host).click());
    const out = Array.from(panel(host).querySelectorAll("button")).find((b: any) => b.textContent === "Sign out") as HTMLButtonElement;
    await act(async () => { out.click(); });
    assert(changed === 0, "a failed logout cannot refresh and silently reopen the workspace");
    assert(sessionKey() === "retained-tab-key", "unknown logout outcome retains the existing credential for verified recovery");
    assert(unavailable === 1, "failed logout reports unavailable instead of confirmed sign-out");
    window.removeEventListener("cadence:session-logout-unavailable", failed);
    setSessionKey(null);
    Object.defineProperty(globalThis, "fetch", { configurable: true, value: realFetch });
    done();
  }
}
function rest() {
// Only signed_in === true shows the menu: unknown, missing or truthy-but-not-true do not.
for (const value of [null, undefined, "true", 1]) {
  const { host, done } = mount(menu({ ...signedIn, signed_in: value } as unknown as Meta));
  assert(!host.querySelector("[data-account-menu]"), `signed_in=${String(value)} renders no menu`);
  done();
}
// Signed out: no account menu, the SignIn disclosure stays.
{
  const { host, done } = mount(createElement("div", null, menu(signedOut), createElement(SignIn, { meta: signedOut, onChange: () => undefined })));
  assert(!host.querySelector("[data-account-menu]"), "no account menu when signed out");
  assert(host.querySelector("details summary")?.textContent?.includes("Sign in"), "SignIn disclosure present");
  done();
}
{
  const readonlyVisitor = { ...signedOut, read_only: true, anonymous_reads: true };
  const { host, done } = mount(createElement(SignIn, { meta: readonlyVisitor, onChange: () => undefined }));
  assert(host.querySelector("details summary"), "explicit anonymous read-only browsing still offers session sign-in");
  assert(!host.textContent?.includes("Sign in to send messages"), "read-only policy is never presented as a promise that signing in enables workspace writes");
  done();
}
{
  const { host, done } = mount(createElement("div", null, menu(signedIn), createElement(SignIn, { meta: signedIn, onChange: () => undefined })));
  assert(host.querySelector("[data-account-menu]") && !host.querySelector("details"), "signed in: menu, no SignIn chip");
  done();
}
{
  const { host, done } = mount(createElement(SignIn, { meta: signedIn, onChange: () => undefined }));
  assert(host.innerHTML === "" && !/sign out/i.test(host.textContent ?? ""), "SignIn renders nothing when signed in");
  done();
}
}
signOutCase().then(guardCases).then(() => { rest(); console.log("account menu checks passed"); }, (e) => { console.error(e); require("process").exit(1); });
