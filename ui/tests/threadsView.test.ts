/** CAD-818 Threads: pure view rules (list filters, deep links, sender labels, routed copies, delivery stages). */
import {
  conversationTitle,
  deliveryLabel,
  kindFilterMatches,
  operatorDmAlias,
  participantLabel,
  readThreadsUrl,
  threadsUrlSearch,
} from "../src/features/threads/threadsView";
import type { ConversationRow } from "../src/features/threads/threadsClient";

function equal(actual: unknown, expected: unknown, what: string): void {
  const a = JSON.stringify(actual);
  const e = JSON.stringify(expected);
  if (a !== e) throw new Error(`${what}: expected ${e}, got ${a}`);
}
function assert(value: unknown, why: string): asserts value {
  if (!value) throw new Error(why);
}

const agent = (uid: string, alias: string, removed = false) => ({ uid, alias, kind: "agent" as const, removed });
const unknownSender = { uid: null, alias: null, kind: "unknown" as const, removed: false };

// Deep links: the selected conversation and the filters survive a reload; defaults drop out.
const state = readThreadsUrl("?c=conv-abc&kind=groups&project=cadence&agent=pm&system=1");
equal(state, { conversation: "conv-abc", kind: "groups", project: "cadence", agent: "pm", includeSystem: true }, "deep link parses");
equal(threadsUrlSearch(state), "?c=conv-abc&kind=groups&project=cadence&agent=pm&system=1", "state round-trips");
equal(threadsUrlSearch({ kind: "all", includeSystem: false }), "", "defaults drop out of the link");
equal(readThreadsUrl("?kind=bogus&system=0").kind, "all", "an unknown kind falls back to all, never an error");
equal(readThreadsUrl("?c=%20").conversation, null, "a blank conversation id is no selection");

// Filters: all / agents (pairs) / operator / groups.
assert(kindFilterMatches("all", "group"), "all shows groups");
assert(kindFilterMatches("agents", "pair") && !kindFilterMatches("agents", "operator"), "agents shows pairs only");
assert(kindFilterMatches("operator", "operator") && !kindFilterMatches("operator", "pair"), "operator shows You<->agent only");
assert(kindFilterMatches("groups", "group") && !kindFilterMatches("groups", "pair"), "groups shows groups only");

// Sender labels: unknown sender for legacy rows, "(removed)" keeps history readable.
equal(participantLabel(unknownSender), "Unknown sender", "legacy row with no recorded sender");
equal(participantLabel(agent("agt-1", "pm", true)), "pm (removed)", "removed participant keeps its alias");
equal(participantLabel(agent("agt-2", "worker")), "worker", "a live agent shows its alias");
equal(participantLabel({ uid: "operator", alias: null, kind: "operator", removed: false }), "You", "operator label");

// Title: the participant list, never an id.
const pair: ConversationRow = {
  id: "conv-1",
  kind: "pair",
  participants: [agent("agt-1", "pm"), agent("agt-2", "worker", true)],
  last: null,
  counts: { queued: 0, claimed: 0, running: 0, completed: 0, failed: 0, unknown: 0 },
  last_activity: 0,
};
equal(conversationTitle(pair), "pm ↔ worker (removed)", "title names participants and marks removed");

// Operator DM alias: the agent side only, and never for a group or a pair.
equal(operatorDmAlias({ ...pair, kind: "operator", participants: [{ uid: "operator", alias: null, kind: "operator", removed: false }, agent("agt-1", "pm")] }), "pm", "operator DM goes to the agent");
equal(operatorDmAlias(pair), null, "a pair has no operator DM");
equal(operatorDmAlias({ ...pair, kind: "operator", participants: [{ uid: "operator", alias: null, kind: "operator", removed: false }, agent("agt-1", "pm", true)] }), null, "a removed agent has no DM");

// Delivery stage text: queued is never reported as delivered or seen.
equal(deliveryLabel("queued"), "Queued", "queued is queued");
equal(deliveryLabel("completed"), "Completed", "completed is completed");
equal(deliveryLabel("made-up"), "Unknown", "an unrecognized stage is unknown, not delivered");
assert(deliveryLabel("queued") !== "Delivered" && deliveryLabel("queued") !== "Seen", "queued is never shown as delivered or seen");

console.log("threads view checks passed");
