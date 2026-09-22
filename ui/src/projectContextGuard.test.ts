import { responseBelongsToRequest, visibleContext } from "./projectContextGuard";

function equal(actual: unknown, expected: unknown): void {
  if (actual !== expected) {
    throw new Error(`expected ${String(expected)}, got ${String(actual)}`);
  }
}

// A's late success cannot overwrite B after the generation advances.
equal(responseBelongsToRequest(1, 2, "beta", "alpha"), false);
equal(responseBelongsToRequest(2, 2, "beta", "beta"), true);

const alpha = { project: "alpha", state: "ready" };
equal(visibleContext("beta", alpha), null);
equal(visibleContext("alpha", alpha), alpha);

console.log("project context guard checks passed");
