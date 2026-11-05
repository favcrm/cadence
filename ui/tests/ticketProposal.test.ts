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
check(parseTicketProposal({ ...proposal, tools: [] })?.tools.length === 0, "an empty tool list is a real answer");
check(parseTicketProposal({ ...proposal, visibility: "private" }) === null, "an audience the card cannot state makes no proposal");
check(parseTicketProposal(null) === null, "no payload, no proposal");

// The action is create (idempotency key = the card id) then assign at the
// created revision. A replay of the create is answered by the board with the
// ticket's current revision and `replayed`, so it must not patch again.
const creates: { idempotency_key: string; project: string; title: string }[] = [];
const patches: { id: string; owner: string; ifRev?: string }[] = [];
let createsSeen = 0;
const client = {
  create: async (req: { project: string; title: string; body: string; idempotency_key: string }) => {
    creates.push({ idempotency_key: req.idempotency_key, project: req.project, title: req.title });
    createsSeen += 1;
    return createsSeen === 1
      ? { issue: { id: "DEMO-1" }, card: { rev: "r1" }, replayed: false }
      : {
          issue: { id: "DEMO-1", owner: "agents/ap_1" },
          card: { rev: "r2", owner: "agents/ap_1" },
          replayed: true,
        };
  },
  patch: async (id: string, patch: { owner: string }, ifRev?: string) => {
    patches.push({ id, owner: patch.owner, ifRev });
    return {};
  },
  issue: async () => ({ owner: null }),
};

async function main(): Promise<void> {
  const first = await createAndAssign(client, parsed!);
  const replay = await createAndAssign(client, parsed!);
  check(first.ticket === "DEMO-1" && first.state === "assigned", "the first click assigns");
  check(replay.ticket === "DEMO-1" && replay.state === "replayed", "a replayed card reports instead of patching");
  check(creates.length === 2 && creates.every((c) => c.idempotency_key === "c-0001" && c.project === "demo"), "every create is keyed by the card id");
  check(patches.length === 1, "only the fresh create patches");
  check(patches[0].id === "DEMO-1" && patches[0].owner === "agents/ap_1" && patches[0].ifRev === "r1", "the patch names the agent at the created revision");

  // A ticket that already has an owner is reported, never reassigned — even
  // when the create was not itself a replay.
  const owned = {
    create: async () => ({ issue: { id: "DEMO-2", owner: "user_owner" }, card: { rev: "r9", owner: "user_owner" } }),
    patch: async () => {
      throw new Error("an owned ticket must not be patched");
    },
    issue: async () => ({ owner: "user_owner" }),
  };
  const ownedOutcome = await createAndAssign(owned, parsed!);
  check(ownedOutcome.ticket === "DEMO-2" && ownedOutcome.state === "owned", "an owned ticket is reported");

  // The create committed and the assign was refused: the card still names the
  // ticket and does not claim the agent was not assigned.
  const refused = {
    create: async () => ({ issue: { id: "DEMO-3" }, card: { rev: "r1" } }),
    patch: async () => {
      throw new Error("The owner names an agent profile that does not exist or is archived.");
    },
    issue: async () => ({ owner: null }),
  };
  const failed = await createAndAssign(refused, parsed!);
  check(failed.ticket === "DEMO-3" && failed.state === "unconfirmed", "an unanswered assign reports the created ticket as unconfirmed");
  check(failed.state === "unconfirmed" && /archived/.test(failed.error ?? ""), "the board's message is kept");

  // The PATCH committed but its answer was lost: the ticket says so.
  const landed = {
    create: async () => ({ issue: { id: "DEMO-4" }, card: { rev: "r1" } }),
    patch: async () => {
      throw new Error("network reset");
    },
    issue: async () => ({ owner: "agents/ap_1" }),
  };
  const settled = await createAndAssign(landed, parsed!);
  check(settled.state === "assigned", "a lost answer is settled by reading the ticket");
}

void main();
