/** Master's ticket proposal in the thread (AOS-517 / CAD-2188 / CAD-2189).
 *
 *  The board projects a `ticket_proposal` entry after the `propose_ticket`
 *  tool result. Its payload is the whole card, so this module only validates
 *  it and performs the one confirmed write. On a board with the CAD-2189 route
 *  that write is one call answered by a durable receipt, so a retried click
 *  finishes an unfinished assignment and never reopens a person's decision; a
 *  board without it keeps the older create (the proposal id is the idempotency
 *  key) then assign pair. Nothing here runs until the person clicks. */

export type TicketProposal = {
  /** The card's own id; also the create's idempotency key. */
  id: string;
  project: string;
  title: string;
  brief: string;
  agent: { id: string; name: string };
  tools: string[];
  /** Who can read the ticket once it exists. Only the company-wide card is known. */
  visibility: "company";
  /** Set by the board once the card's ticket exists; `run` once its run ended. */
  ticket?: string;
  run?: { state: string; reason: string | null; agent: string | null };
};

const text = (value: unknown): string | null =>
  typeof value === "string" && value.trim() !== "" ? value : null;

/** Null when the payload is not a complete proposal; the card then shows the
 *  entry's plain text instead of a broken action. */
export function parseTicketProposal(
  payload: Record<string, unknown> | null | undefined,
): TicketProposal | null {
  if (!payload) return null;
  const id = text(payload.id);
  const project = text(payload.project);
  const title = text(payload.title);
  const brief = text(payload.brief);
  const agent = payload.agent;
  if (!id || !project || !title || !brief) return null;
  if (agent === null || typeof agent !== "object" || Array.isArray(agent)) return null;
  const record = agent as { id?: unknown; name?: unknown };
  const agentId = text(record.id);
  const agentName = text(record.name);
  if (!agentId || !agentName) return null;
  // An audience this card did not design for is not drawn as company-visible.
  if (payload.visibility !== "company") return null;
  // The board's read-time join: a ticket id and an ended run state, both or
  // neither. A running job sends no state and the card stays actionable.
  const ticket = text(payload.ticket);
  const run = payload.run;
  let outcome: { state: string; reason: string | null; agent: string | null } | undefined;
  if (run !== null && typeof run === "object" && !Array.isArray(run)) {
    const record = run as { state?: unknown; reason?: unknown; agent?: unknown };
    if (typeof record.state === "string" && record.state !== "")
      outcome = {
        state: record.state,
        reason: typeof record.reason === "string" ? record.reason : null,
        // The run's own agent; null when the profile no longer resolves.
        agent: typeof record.agent === "string" && record.agent !== "" ? record.agent : null,
      };
  }
  // The tool list is part of what the person authorises: a list that is not
  // all names would make the card claim capabilities it cannot state, so no
  // card at all. An empty list is a real answer and is shown as none.
  const tools = payload.tools;
  if (
    !Array.isArray(tools) ||
    !tools.every((tool) => typeof tool === "string" && tool !== "")
  )
    return null;
  return {
    id,
    project,
    title,
    brief,
    agent: { id: agentId, name: agentName },
    tools,
    visibility: "company",
    ...(ticket !== null && outcome !== undefined ? { ticket, run: outcome } : {}),
  };
}

/** The board calls this card may make, narrowed so a test can supply them:
 *  one start on a new board, or the older create + patch + settle pair. */
export type TicketProposalClient = {
  /**
   * CAD-2189: the board's one-step start. Present only on a board that has it;
   * an older board is detected from the refusal, never assumed.
   */
  start?: (
    proposal: string,
    req: { project: string; title: string; body: string; agent: string },
  ) => Promise<ProposalStartAnswer>;
  create: (req: {
    project: string;
    title: string;
    body: string;
    idempotency_key: string;
  }) => Promise<{
    issue: { id: string; owner?: string | null };
    card?: { rev?: string; owner?: string | null };
    replayed?: boolean;
  }>;
  patch: (id: string, patch: { owner: string }, ifRev?: string) => Promise<unknown>;
  /** Re-read one ticket, to settle an assign whose answer was lost. */
  issue: (id: string) => Promise<{ owner?: string | null }>;
};

/** The board's durable receipt for one proposal (CAD-2189). */
export type ProposalReceipt = {
  proposal: string;
  ticket: string;
  /** The one owner the assignment writes, `agents/ap_…`. */
  agent: string;
  state: "pending" | "assigned" | "refused";
  /** The card's owner as stored now: the only field that proves the assignment. */
  owner: string | null;
  /** Why a `refused` receipt is refused; null otherwise. */
  reason: string | null;
  at: number;
};

/** The one-step start's answer: the card, and the receipt when the board has it. */
export type ProposalStartAnswer = {
  issue: { id: string; owner?: string | null };
  card?: { rev?: string; owner?: string | null };
  receipt?: ProposalReceipt;
  replayed?: boolean;
};

/** Create the ticket, then assign it to the agent unless the card already did.
 *
 *  A board with the CAD-2189 route answers both facts at once and with a
 *  durable receipt: a lost answer leaves a provable state, so a retry finishes
 *  an unfinished assignment and never reopens a person's decision. A board
 *  without it keeps the older create + patch pair, whose replay rule is that a
 *  create the board reports as replayed is reported and not patched again. */
export type CreateAndAssignOutcome =
  | { ticket: string; state: "assigned" }
  /** The ticket exists but this card did not (or cannot) assign it. */
  | { ticket: string; state: "replayed" | "owned" | "unconfirmed"; error?: string }
  /** Created; the board proved no owner was written, so a retry may finish it. */
  | { ticket: string; state: "pending" }
  /** Assigned once, but the card's owner is now someone else's or nobody's. */
  | { ticket: string; state: "changed"; owner: string | null }
  /** The board refused the assignment and recorded why; nothing was written. */
  | { ticket: string; state: "refused"; reason: string; error?: string };

export async function createAndAssign(
  client: TicketProposalClient,
  proposal: TicketProposal,
): Promise<CreateAndAssignOutcome> {
  if (client.start) {
    try {
      const answer = await client.start(proposal.id, {
        project: proposal.project,
        title: proposal.title,
        body: proposal.brief,
        agent: proposal.agent.id,
      });
      return startOutcome(proposal, answer);
    } catch (error) {
      // Only a board that lacks the route or the hub method falls back. Any
      // other failure may have committed, and a create would then compound it
      // instead of letting the card's retry finish from the receipt.
      if (!olderBoard(error)) throw error;
    }
  }
  return createThenAssign(client, proposal);
}

/**
 * Whether a refusal means "this board cannot start a proposal in one step":
 * the route is absent (a bare 404 with no code, or 405) or the hub has no such
 * method (501 / `unsupported`). A coded 404 is the board's own answer and is
 * not an older board.
 */
function olderBoard(error: unknown): boolean {
  const refusal = error as { status?: unknown; code?: unknown } | null;
  if (!refusal || typeof refusal !== "object") return false;
  if (refusal.status === 405 || refusal.status === 501) return true;
  return refusal.status === 404 && (refusal.code === undefined || refusal.code === "unsupported");
}

/**
 * Read the receipt, never the request, as the assignment's evidence. Three
 * things can be true and only the receipt tells them apart: this call assigned
 * it, the ticket was already owned so this call left it alone, or the board
 * created the ticket and refused the assignment.
 */
function startOutcome(proposal: TicketProposal, answer: ProposalStartAnswer): CreateAndAssignOutcome {
  const ticket = answer.issue.id;
  const receipt = answer.receipt;
  // No receipt, or a receipt naming another ticket, proves nothing: the older
  // create-only truth is "the ticket exists, the assignment is unconfirmed".
  if (!receipt || receipt.ticket !== ticket) return { ticket, state: "unconfirmed" };
  const expected = `agents/${proposal.agent.id}`;
  if (receipt.owner === expected && receipt.state === "assigned") return { ticket, state: "assigned" };
  // The assignment landed once and a person has since moved or cleared it:
  // the card reports the choice it did not make.
  if (receipt.state === "assigned") return { ticket, state: "changed", owner: receipt.owner };
  if (receipt.state === "pending") return { ticket, state: "pending" };
  if (receipt.reason === "owner_set") return { ticket, state: "owned" };
  if (receipt.reason === "untracked_existing")
    return { ticket, state: "unconfirmed", error: "untracked_existing" };
  if (receipt.reason === "ticket_missing") return { ticket, state: "unconfirmed" };
  return { ticket, state: "refused", reason: receipt.reason ?? "refused" };
}

/** The pre-CAD-2189 pair, kept for boards that answer `unsupported`. */
async function createThenAssign(
  client: TicketProposalClient,
  proposal: TicketProposal,
): Promise<CreateAndAssignOutcome> {
  const created = await client.create({
    project: proposal.project,
    title: proposal.title,
    body: proposal.brief,
    idempotency_key: proposal.id,
  });
  const ticket = created.issue.id;
  const owner = created.card?.owner ?? created.issue.owner ?? null;
  if (created.replayed === true) return { ticket, state: "replayed" };
  if (owner !== null) return { ticket, state: "owned" };
  const agentOwner = `agents/${proposal.agent.id}`;
  try {
    await client.patch(ticket, { owner: agentOwner }, created.card?.rev);
    return { ticket, state: "assigned" };
  } catch (error) {
    // A rejected call can mean the PATCH committed and its answer was lost, so
    // the ticket is the only witness: read it before claiming what happened.
    const message = error instanceof Error ? error.message : String(error);
    try {
      const current = await client.issue(ticket);
      if (current.owner === agentOwner) return { ticket, state: "assigned" };
    } catch {
      // Unreadable is unconfirmed, never "not assigned".
    }
    return { ticket, state: "unconfirmed", error: message };
  }
}
