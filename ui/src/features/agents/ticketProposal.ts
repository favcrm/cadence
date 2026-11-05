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
  const tools = Array.isArray(payload.tools)
    ? payload.tools.filter((tool): tool is string => typeof tool === "string")
    : [];
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
  }) => Promise<{ issue: { id: string }; card?: { rev?: string } }>;
  patch: (id: string, patch: { owner: string }, ifRev?: string) => Promise<unknown>;
};

/** Create the ticket, then assign it to the agent. The same proposal id and
 *  the same `if_rev` make a retry of either half a repeat, not a second write. */
export async function createAndAssign(
  client: TicketProposalClient,
  proposal: TicketProposal,
): Promise<{ ticket: string }> {
  const created = await client.create({
    project: proposal.project,
    title: proposal.title,
    body: proposal.brief,
    idempotency_key: proposal.id,
  });
  await client.patch(
    created.issue.id,
    { owner: `agents/${proposal.agent.id}` },
    created.card?.rev,
  );
  return { ticket: created.issue.id };
}
