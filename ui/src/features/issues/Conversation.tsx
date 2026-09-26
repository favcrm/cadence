/**
 * Conversation slot (CAD-607). CAD-608 replaces this body with the lane
 * thread, the status/instruction composer, and Ask master. The page only
 * chooses when the slot is on screen.
 */
export interface ConversationSlotProps {
  issueId: string;
  agentAlias: string | null;
}

export default function ConversationSlot({ issueId, agentAlias }: ConversationSlotProps) {
  return (
    <section className="card px-4 py-8 text-center" data-slot="conversation" aria-label="Conversation">
      <div className="text-cardtitle font-semibold text-ink-100">
        {agentAlias ? "Lane conversation" : "No conversation yet"}
      </div>
      <p className="text-secondary text-ink-400 mt-1 max-w-prose mx-auto">
        {agentAlias
          ? `${agentAlias}’s thread, status nudges, and instructions land in this slot.`
          : "The lane agent’s thread shows up here after kick off."}
      </p>
      <p className="num text-micro text-ink-500 mt-2">{issueId}</p>
    </section>
  );
}
