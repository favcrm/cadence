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
equal(readThemePref(storage), "dark", "nothing stored → dark default");
writeThemePref("light", storage);
equal(readThemePref(storage), "light", "stored light");
writeThemePref("system", storage);
equal(readThemePref(storage), "system", "system is stored explicitly, not cleared");
store.set("cadence-theme", "sepia");
equal(readThemePref(storage), "dark", "unknown value → dark default");
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
equal(readThemePref(blocked), "dark", "blocked storage reads as the dark default");
writeThemePref("dark", blocked); // must not throw
equal(readThemePref(undefined), "dark", "no storage → dark default");
equal(nextThemePref("system"), "light", "cycle 1");
equal(nextThemePref("light"), "dark", "cycle 2");
equal(nextThemePref("dark"), "system", "cycle 3");

console.log("theme checks passed");
