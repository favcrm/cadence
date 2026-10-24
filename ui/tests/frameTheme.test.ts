export {};
/** The framed app must follow the board's pick, not the OS (SOC-19).
 * A sandboxed frame is its own document: it never sees the board's
 * `data-theme`, so the applied pick is read here and carried in the mount. */
declare function require(name: string): any;
const { Window } = require("happy-dom");
const win = new Window({ url: "http://localhost/apps/social-content" });
for (const name of ["document", "MutationObserver", "Element"]) Object.defineProperty(globalThis, name, { value: win[name], configurable: true, writable: true });
const { appliedFrameTheme, applyThemePref, onAppliedThemeChange, initTheme } = require("../src/lib/theme") as typeof import("../src/lib/theme");

function assert(value: unknown, why: string): asserts value { if (!value) throw new Error(why); }
const html = document.documentElement;

// An explicit pick is the value the frame must be told.
applyThemePref("light");
assert(appliedFrameTheme() === "light", "an explicit light pick reaches the frame");
applyThemePref("dark");
assert(appliedFrameTheme() === "dark", "an explicit dark pick reaches the frame");

// "system" removes the attribute, so the frame gets no theme and its own
// media query decides — the same rule the board follows.
applyThemePref("system");
assert(html.getAttribute("data-theme") === null, "system leaves no data-theme");
assert(appliedFrameTheme() === null, "system sends no theme with the mount");

// A change while a screen is open must be observable (the outlet re-mints).
const seen: (string | null)[] = [];
const stop = onAppliedThemeChange(theme => seen.push(theme));
applyThemePref("light");
applyThemePref("system");
stop();
applyThemePref("dark");
assert(JSON.stringify(seen) === JSON.stringify(["light", null]), `observed ${JSON.stringify(seen)}`);
assert(seen.length === 2, "an unsubscribed observer stops reporting");

// A fresh board applies its stored pick (dark by default) before the first render.
assert(["light", "dark", "system"].includes(initTheme()), "initTheme returns a stored pick");
console.log("frameTheme: ok");
