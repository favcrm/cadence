/** Master's ticket proposal in the thread (AOS-517 / CAD-2188).
 *
 *  The board projects a `ticket_proposal` entry after the `propose_ticket`
 *  tool result. Its payload is the whole card, so this module only validates
 *  it and performs the one confirmed write: create the ticket (the proposal id
 *  is the idempotency key, so a retry cannot mint a second one) and then assign
 *  it to the agent profile. Nothing here runs until the person clicks. */

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
  };
}

/** The two board writes this card makes, narrowed so a test can supply them. */
export type TicketProposalClient = {
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
};

/** Create the ticket, then assign it to the agent unless the card already did.
 *  The create is idempotent by card id: a retry answers the ticket made the
 *  first time with its *current* revision and `replayed: true`, so patching it
 *  would overwrite a reassignment or a cleared owner. A replay, or a ticket
 *  that already has an owner, is reported and left alone. */
export type CreateAndAssignOutcome =
  | { ticket: string; state: "assigned" }
  /** The ticket exists but this card did not (or cannot) assign it. */
  | { ticket: string; state: "replayed" | "owned" | "failed"; error?: string };

export async function createAndAssign(
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
  try {
    await client.patch(ticket, { owner: `agents/${proposal.agent.id}` }, created.card?.rev);
    return { ticket, state: "assigned" };
  } catch (error) {
    // The ticket is committed and company-visible; say so rather than showing
    // only the board's refusal and leaving it unmentioned.
    return {
      ticket,
      state: "failed",
      error: error instanceof Error ? error.message : String(error),
    };
  }
}
