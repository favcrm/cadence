import { nextThemeFor, nextThemePref, readThemePref, writeThemePref } from "../src/lib/theme";

function equal(actual: unknown, expected: unknown, what: string): void {
  if (actual !== expected) throw new Error(`${what}: expected ${String(expected)}, got ${String(actual)}`);
}

const store = new Map<string, string>();
const storage = {
  getItem: (k: string) => store.get(k) ?? null,
  setItem: (k: string, v: string) => void store.set(k, v),
  removeItem: (k: string) => void store.delete(k),
};
equal(readThemePref(storage), "system", "nothing stored → system default");
writeThemePref("light", storage);
equal(readThemePref(storage), "light", "stored light");
writeThemePref("system", storage);
equal(readThemePref(storage), "system", "system is stored explicitly, not cleared");
store.set("cadence-theme", "sepia");
equal(readThemePref(storage), "system", "unknown value → system default");
const blocked = {
  getItem: () => {
    throw new Error("blocked");
  },
  setItem: () => {
    throw new Error("blocked");
  },
  removeItem: () => {
    throw new Error("blocked");
  },
};
equal(readThemePref(blocked), "system", "blocked storage reads as the system default");
writeThemePref("dark", blocked); // must not throw
equal(readThemePref(undefined), "system", "no storage → system default");
equal(nextThemePref("system"), "light", "cycle 1");
equal(nextThemePref("light"), "dark", "cycle 2");
equal(nextThemePref("dark"), "system", "cycle 3");
// The successor is the opposite of what is rendered, whatever the pref says —
// an explicit light on a light OS is reachable from the account menu and must
// still change on click.
equal(nextThemeFor("system", true), "light", "system renders dark → light");
equal(nextThemeFor("system", false), "dark", "system renders light → dark");
equal(nextThemeFor("light", true), "dark", "explicit light renders light → dark");
equal(nextThemeFor("light", false), "dark", "explicit light on a light OS → dark");
equal(nextThemeFor("dark", true), "light", "explicit dark renders dark → light");
equal(nextThemeFor("dark", false), "light", "explicit dark on a light OS → light");

console.log("theme checks passed");
