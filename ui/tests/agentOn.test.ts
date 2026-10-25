import type { Agent } from "../src/lib/types";
import { agentOn } from "../src/lib/agentOn";

function equal(actual: unknown, expected: unknown, what: string): void {
  if (JSON.stringify(actual) !== JSON.stringify(expected)) {
    throw new Error(`${what}: expected ${JSON.stringify(expected)}, got ${JSON.stringify(actual)}`);
  }
}

const agent = (row: Partial<Agent>) => row as Agent;

equal(agentOn(agent({ on: ["ACME-1", "ACME-2"] })), ["ACME-1", "ACME-2"], "a bound row passes through");
// The hosted tenant sent no `on`; iterating it blanked the board (CAD-1833).
equal(agentOn(agent({ on: undefined })), [], "a row without on has no bound issues");
equal(agentOn(agent({ on: null as never })), [], "a null on is not a list");
equal(agentOn(agent({ on: "ACME-1" as never })), [], "a non-array on is not a list");

console.log("agent-on checks passed");
