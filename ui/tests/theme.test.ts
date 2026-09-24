import { nextThemePref, readThemePref, writeThemePref } from "../src/lib/theme";

function equal(actual: unknown, expected: unknown, what: string): void {
  if (actual !== expected) throw new Error(`${what}: expected ${String(expected)}, got ${String(actual)}`);
}

const store = new Map<string, string>();
const storage = {
  getItem: (k: string) => store.get(k) ?? null,
  setItem: (k: string, v: string) => void store.set(k, v),
  removeItem: (k: string) => void store.delete(k),
};
equal(readThemePref(storage), "system", "nothing stored → system");
writeThemePref("light", storage);
equal(readThemePref(storage), "light", "stored light");
writeThemePref("system", storage);
equal(store.size, 0, "system clears the pick");
store.set("cadence-theme", "sepia");
equal(readThemePref(storage), "system", "unknown value → system");
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
equal(readThemePref(blocked), "system", "blocked storage reads as system");
writeThemePref("dark", blocked); // must not throw
equal(readThemePref(undefined), "system", "no storage");
equal(nextThemePref("system"), "light", "cycle 1");
equal(nextThemePref("light"), "dark", "cycle 2");
equal(nextThemePref("dark"), "system", "cycle 3");

console.log("theme checks passed");
