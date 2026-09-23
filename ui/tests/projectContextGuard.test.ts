import { requestIsCurrent, responseBelongsToRequest, visibleContext } from "../src/features/projects/projectContextGuard";

function equal(actual: unknown, expected: unknown): void {
  if (actual !== expected) {
    throw new Error(`expected ${String(expected)}, got ${String(actual)}`);
  }
}

// A's late success cannot overwrite B after the generation advances.
equal(responseBelongsToRequest(1, 2, "beta", "alpha"), false);
equal(responseBelongsToRequest(2, 2, "beta", "beta"), true);
// A's late error/finally cannot clear B's loading or error state.
equal(requestIsCurrent(1, 2, "beta", "alpha"), false);
equal(requestIsCurrent(2, 2, "beta", "beta"), true);

const alpha = { project: "alpha", state: "ready" };
equal(visibleContext("beta", alpha), null);
equal(visibleContext("alpha", alpha), alpha);
equal(visibleContext("all", alpha), null);
equal(visibleContext("unknown", alpha), null);

console.log("project context guard checks passed");
