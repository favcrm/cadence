import { threadItems, type ThreadEntry, type ThreadState } from "../src/features/home/thread";
import { createAndAssign, parseTicketProposal } from "../src/features/agents/ticketProposal";

function check(condition: boolean, message: string): void {
  if (!condition) throw new Error(`ticketProposal: ${message}`);
}

const entry = (
  seq: number,
  kind: string,
  text: string,
  message = "m1",
  payload?: Record<string, unknown>,
): ThreadEntry => ({ seq, role: "agent", kind, text, message, payload: payload ?? null });

const proposal = {
  id: "c-0001",
  project: "demo",
  title: "Compare our competitors",
  brief: "Compare the 5 competitors in your wiki on pricing.",
  agent: { id: "ap_1", name: "Research agent" },
  tools: ["web_search", "web_read", "ticket_review"],
  visibility: "company",
};

// The board's entry becomes one card item of its own, in order.
const state = {
  entries: [
    entry(1, "assistant_text", "Here is the ticket."),
    entry(2, "ticket_proposal", "", "m1", proposal),
    entry(3, "turn_result", "Done."),
  ],
  pending: [],
} as unknown as ThreadState;
const items = threadItems(state);
const card = items.find((i) => i.type === "ticket");
check(card !== undefined, "a ticket_proposal entry becomes a ticket card item");
check(items.indexOf(card!) === 1, "the card keeps its place between the commentary and the answer");
check(items.some((i) => i.type === "commentary"), "the commentary stays its own line");

// The payload is validated, never trusted: a missing agent or text yields no
// card action, and the component falls back to the entry's plain text.
const parsed = parseTicketProposal(proposal);
check(parsed !== null && parsed.agent.id === "ap_1" && parsed.tools.length === 3, "a complete payload parses");
check(parseTicketProposal({ ...proposal, agent: null }) === null, "no agent, no proposal");
check(parseTicketProposal({ ...proposal, brief: "  " }) === null, "blank brief, no proposal");
check(parseTicketProposal({ ...proposal, tools: ["web_read", 7] }) === null, "an incomplete tool list makes no proposal");
check(parseTicketProposal({ ...proposal, tools: [] }) === null, "an empty tool list makes no proposal");
check(parseTicketProposal({ ...proposal, visibility: "private" }) === null, "an audience the card cannot state makes no proposal");
check(parseTicketProposal(null) === null, "no payload, no proposal");

// The action is create (idempotency key = the card id) then assign at the
// created revision. A retry reuses both, so a double click is one ticket.
const creates: { idempotency_key: string; project: string; title: string }[] = [];
const patches: { id: string; owner: string; ifRev?: string }[] = [];
const client = {
  create: async (req: { project: string; title: string; body: string; idempotency_key: string }) => {
    creates.push({ idempotency_key: req.idempotency_key, project: req.project, title: req.title });
    return { issue: { id: "DEMO-1" }, card: { rev: "r1" } };
  },
  patch: async (id: string, patch: { owner: string }, ifRev?: string) => {
    patches.push({ id, owner: patch.owner, ifRev });
    return {};
  },
};
async function main(): Promise<void> {
  const first = await createAndAssign(client, parsed!);
  const replay = await createAndAssign(client, parsed!);
  check(first.ticket === "DEMO-1" && replay.ticket === "DEMO-1", "both calls resolve the same ticket");
  check(first.assigned && replay.assigned, "the unowned ticket is assigned on each replay");
  check(creates.length === 2 && patches.length === 2, "each call creates once and patches once");
  check(
    creates.every((c) => c.idempotency_key === "c-0001" && c.project === "demo"),
    "every create is keyed by the card id",
  );
  check(
    patches.every((p) => p.id === "DEMO-1" && p.owner === "agents/ap_1" && p.ifRev === "r1"),
    "the patch names the agent at the created revision",
  );

  // The card is replayed after somebody reassigned the ticket: the create
  // answers the ticket with its current revision and an owner, and the card
  // must report it instead of overwriting the newer assignment.
  const owned = {
    create: async () => ({ issue: { id: "DEMO-1", owner: "user_owner" }, card: { rev: "r9", owner: "user_owner" } }),
    patch: async () => {
      throw new Error("a replay must not patch an owned ticket");
    },
  };
  const replayOwned = await createAndAssign(owned, parsed!);
  check(replayOwned.ticket === "DEMO-1" && !replayOwned.assigned, "an owned ticket is reported, not reassigned");
}

void main();
