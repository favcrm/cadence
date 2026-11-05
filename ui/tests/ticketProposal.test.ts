import { threadItems, type ThreadEntry, type ThreadState } from "../src/features/home/thread";
import {
  createAndAssign,
  parseTicketProposal,
  type ProposalReceipt,
  type ProposalStartAnswer,
  type TicketProposalClient,
} from "../src/features/agents/ticketProposal";

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

// The board's read-time join: a ticket and its ended run, both or neither.
const joined = parseTicketProposal({ ...proposal, ticket: "CAD-2188", run: { state: "finished", reason: null, agent: "Research agent" } });
check(joined?.ticket === "CAD-2188" && joined.run?.state === "finished", "an ended run is carried on the card");
check(joined?.run?.agent === "Research agent", "the run's own agent is carried, not the proposal's");
const unnamed = parseTicketProposal({ ...proposal, ticket: "CAD-2188", run: { state: "failed", reason: "turn_failed", agent: null } });
check(unnamed?.run?.agent === null, "an unresolved run agent stays null rather than borrowing the proposal's");
check(parseTicketProposal({ ...proposal, run: { state: "finished", reason: null } })?.ticket === undefined, "a run without a ticket is not shown");
check(parseTicketProposal({ ...proposal, ticket: "CAD-2188" })?.run === undefined, "a ticket without a run is not shown");
check(parseTicketProposal({ ...proposal, ticket: "CAD-2188", run: { state: 7 } })?.run === undefined, "a malformed run is dropped");

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

  // ---- CAD-2189: the board's durable receipt decides what the card claims.
  const receipt = (
    ticket: string,
    state: "pending" | "assigned" | "refused",
    owner: string | null,
    reason: string | null = null,
  ): ProposalReceipt => ({
    proposal: "c-0001",
    ticket,
    agent: "agents/ap_1",
    state,
    owner,
    reason,
    at: 1_000,
  });
  const startOnly = (
    answer: () => ProposalStartAnswer,
    label: string,
  ): TicketProposalClient => ({
    start: async () => answer(),
    create: async () => {
      throw new Error(`${label}: the start route exists, so no create is sent`);
    },
    patch: async () => {
      throw new Error(`${label}: a receipt is the evidence, so no patch is sent`);
    },
    issue: async () => ({ owner: null }),
  });

  // A matching receipt is the evidence that the assignment completed.
  const fresh: TicketProposalClient = {
    start: async () => {
      return { issue: { id: "DEMO-5" }, card: { rev: "r1" }, receipt: receipt("DEMO-5", "assigned", "agents/ap_1") };
    },
    create: async () => {
      throw new Error("fresh: the start route exists, so no create is sent");
    },
    patch: async () => {
      throw new Error("fresh: a receipt is the evidence, so no patch is sent");
    },
    issue: async () => ({ owner: null }),
  };
  const assignedByReceipt = await createAndAssign(fresh, parsed!);
  check(assignedByReceipt.state === "assigned" && assignedByReceipt.ticket === "DEMO-5", "the receipt assigns");

  // The create committed and the answer was lost: the retry finishes from the
  // receipt instead of minting or reassigning anything.
  let attempts = 0;
  const lostThenFinished = {
    start: async () => {
      attempts += 1;
      if (attempts === 1) throw Object.assign(new Error("network reset"), { status: 0 });
      return { issue: { id: "DEMO-6" }, receipt: receipt("DEMO-6", "assigned", "agents/ap_1") };
    },
    create: async () => {
      throw new Error("a failed start is retried on the start route, never by a create");
    },
    patch: async () => {
      throw new Error("no patch follows a start");
    },
    issue: async () => ({ owner: null }),
  };
  let lost: unknown;
  try {
    await createAndAssign(lostThenFinished, parsed!);
  } catch (error) {
    lost = error;
  }
  check(lost !== undefined, "a lost start answer is reported, not invented");
  const finished = await createAndAssign(lostThenFinished, parsed!);
  check(
    finished.state === "assigned" && finished.ticket === "DEMO-6" && attempts === 2,
    "a retry after a lost answer finishes from the board's receipt",
  );

  // A second click while the assignment is still pending is the same ticket:
  // the board proves no owner was written, and the retry completes it.
  let clicks = 0;
  const pendingThenDone = startOnly(() => {
    clicks += 1;
    return {
      issue: { id: "DEMO-7" },
      receipt: receipt("DEMO-7", clicks === 1 ? "pending" : "assigned", clicks === 1 ? null : "agents/ap_1"),
    };
  }, "pending");
  const pending = await createAndAssign(pendingThenDone, parsed!);
  check(pending.state === "pending" && pending.ticket === "DEMO-7", "a pending proposal is created, not assigned");
  const completed = await createAndAssign(pendingThenDone, parsed!);
  check(completed.state === "assigned" && clicks === 2, "a provably pending proposal is finished by a retry");

  // A person moved the card after the assignment: the card reports their
  // choice and claims nothing of its own.
  const reassigned = startOnly(
    () => ({ issue: { id: "DEMO-8" }, receipt: receipt("DEMO-8", "assigned", "user_owner") }),
    "reassigned",
  );
  const moved = await createAndAssign(reassigned, parsed!);
  check(moved.state === "changed", "a reassignment is reported, not overwritten");
  check(moved.state === "changed" && moved.owner === "user_owner", "the reported owner is the person's");

  const cleared = startOnly(
    () => ({ issue: { id: "DEMO-9" }, receipt: receipt("DEMO-9", "assigned", null) }),
    "cleared",
  );
  const emptied = await createAndAssign(cleared, parsed!);
  check(emptied.state === "changed", "a cleared owner is reported, not restored");
  check(emptied.state === "changed" && emptied.owner === null, "a cleared owner stays cleared");

  // The board refused the actor: created, refused, and evidence of why.
  const denied = startOnly(
    () => ({
      issue: { id: "DEMO-10" },
      receipt: receipt("DEMO-10", "refused", null, "agent_not_permitted"),
    }),
    "denied",
  );
  const deniedOutcome = await createAndAssign(denied, parsed!);
  check(
    deniedOutcome.state === "refused" && deniedOutcome.ticket === "DEMO-10",
    "a denied actor is a recorded refusal, not an assignment",
  );
  check(
    deniedOutcome.state === "refused" && deniedOutcome.reason === "agent_not_permitted",
    "the refusal names the authority the board applied",
  );

  const alreadyOwned = startOnly(
    () => ({ issue: { id: "DEMO-11" }, receipt: receipt("DEMO-11", "refused", "user_owner", "owner_set") }),
    "owner_set",
  );
  check((await createAndAssign(alreadyOwned, parsed!)).state === "owned", "an owner the board already held is reported");

  const oldTicket = startOnly(
    () => ({ issue: { id: "DEMO-12" }, receipt: receipt("DEMO-12", "refused", null, "untracked_existing") }),
    "untracked_existing",
  );
  const oldOutcome = await createAndAssign(oldTicket, parsed!);
  check(oldOutcome.state === "unconfirmed" && oldOutcome.error === "untracked_existing",
    "a pre-receipt ticket does not claim whether assignment happened");

  const changedBeforeStart = startOnly(
    () => ({ issue: { id: "DEMO-13" }, receipt: receipt("DEMO-13", "refused", null, "ticket_changed") }),
    "ticket_changed",
  );
  const changedOutcome = await createAndAssign(changedBeforeStart, parsed!);
  check(changedOutcome.state === "refused" && changedOutcome.reason === "ticket_changed",
    "an intervening ticket change is reported as a refusal");

  // A stale revision is the caller's fence: it surfaces, and is never turned
  // into a create that would write around the fence.
  const stale = {
    start: async () => {
      throw Object.assign(new Error("if_rev does not match the stored revision"), {
        status: 409,
        code: "if_rev_conflict",
      });
    },
    create: async () => {
      throw new Error("a stale revision is not retried as a create");
    },
    patch: async () => {
      throw new Error("a stale revision is not retried as a patch");
    },
    issue: async () => ({ owner: null }),
  };
  let staleError: unknown;
  try {
    await createAndAssign(stale, parsed!);
  } catch (error) {
    staleError = error;
  }
  check((staleError as { code?: string } | null)?.code === "if_rev_conflict", "a stale revision surfaces");

  // An answer without a receipt, or a receipt for another ticket, proves
  // nothing: the card reports creation and stops short of claiming the agent.
  const noReceipt = startOnly(() => ({ issue: { id: "DEMO-12" } }), "no-receipt");
  check((await createAndAssign(noReceipt, parsed!)).state === "unconfirmed", "no receipt, no assignment claim");
  const otherTicket = startOnly(
    () => ({ issue: { id: "DEMO-13" }, receipt: receipt("DEMO-99", "assigned", "agents/ap_1") }),
    "other-ticket",
  );
  check(
    (await createAndAssign(otherTicket, parsed!)).state === "unconfirmed",
    "a receipt bound to another ticket proves nothing",
  );

  // The board predates the route (501) or lacks it entirely (a bare 404): the
  // card keeps its create + patch pair. Any other failure is not that cue.
  for (const status of [501, 404] as const) {
    let creates = 0;
    const older = {
      start: async () => {
        throw Object.assign(new Error(`${status}`), status === 501 ? { status, code: "unsupported" } : { status });
      },
      create: async () => {
        creates += 1;
        return { issue: { id: "DEMO-14" }, card: { rev: "r1" } };
      },
      patch: async () => ({}),
      issue: async () => ({ owner: null }),
    };
    const fallback = await createAndAssign(older, parsed!);
    check(
      fallback.state === "assigned" && fallback.ticket === "DEMO-14" && creates === 1,
      `an older board (${status}) falls back to create + assign`,
    );
  }

  const unavailable = {
    start: async () => {
      throw Object.assign(new Error("The board read model is unavailable."), {
        status: 503,
        code: "capability_unavailable",
      });
    },
    create: async () => {
      throw new Error("a 503 is not an older board");
    },
    patch: async () => {
      throw new Error("a 503 is not an older board");
    },
    issue: async () => ({ owner: null }),
  };
  let unavailableError: unknown;
  try {
    await createAndAssign(unavailable, parsed!);
  } catch (error) {
    unavailableError = error;
  }
  check(unavailableError !== undefined, "a 503 surfaces instead of being retried as an older board");
}

void main();
