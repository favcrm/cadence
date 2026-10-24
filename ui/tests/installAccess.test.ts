/** Only an active install has an openable app route. */
import { canOpenInstall } from "../src/features/explorer/installAccess";

function equal(actual: unknown, expected: unknown, label: string) {
  if (actual !== expected) throw new Error(`${label}: ${String(actual)} !== ${String(expected)}`);
}

equal(canOpenInstall("active"), true, "active opens");
equal(canOpenInstall(undefined), true, "an older row without the word is active");
equal(canOpenInstall("disabled"), false, "disabled has no route");
equal(canOpenInstall("removed"), false, "removed has no route");
equal(canOpenInstall("active", true), false, "a removed flag wins over a stale active word");

console.log("install access checks pass");
