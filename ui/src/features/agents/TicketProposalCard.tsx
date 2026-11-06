import { useState } from "react";
import { api, ApiError } from "../../lib/api";
import { useTranslate } from "../../lib/locale";
import { resources } from "../../lib/resources";
import Button from "../../ui/Button";
import type { ThreadEntry } from "../home/thread";
import { createAndAssign, parseTicketProposal, type CreateAndAssignOutcome } from "./ticketProposal";

/** The ended run states the board joins onto the card (CAD-2190), in words.
 *  A state this map does not know shows the agent without a claim. */
const RUN_LABEL: Record<string, string> = {
  finished: "finished",
  no_report: "finished without a report",
  failed: "failed",
  refused: "refused",
  stopped: "stopped",
  parked: "paused",
};

/** Master's start card (AOS-517): the whole ticket it proposes, the agent that
 *  would run it, the tools that agent gets, and who can read the result. One
 *  click creates the ticket and assigns it; nothing exists before the click and
 *  a second click starts nothing. */
export default function TicketProposalCard({
  entry,
  onOpenIssue,
  readOnly = false,
}: {
  entry: ThreadEntry;
  onOpenIssue?: (id: string) => void;
  /** A read-only board refuses both writes; do not offer the action. */
  readOnly?: boolean;
}) {
  const t = useTranslate();
  const proposal = parseTicketProposal(entry.payload);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [created, setCreated] = useState<CreateAndAssignOutcome | null>(null);

  if (!proposal) {
    return (
      <p className="text-micro text-ink-500" data-kind="ticket-proposal">
        · {entry.text}
      </p>
    );
  }

  const runLabel = proposal?.run ? RUN_LABEL[proposal.run.state] : undefined;
  const run = () => {
    setBusy(true);
    setError(null);
    createAndAssign(
      {
        ...api,
        issue: (id: string) => api.issue(id),
        // CAD-2189: one write on a board that has it. An older board refuses
        // this call and the pair above runs instead.
        start: (id, req) => api.startProposal(id, req),
      },
      proposal,
    )
      .then((outcome) => {
        setCreated(outcome);
        void resources.issues.invalidate();
        void resources.issue(outcome.ticket).invalidate();
      })
      .catch((e: ApiError) => setError(e.message ?? String(e)))
      .finally(() => setBusy(false));
  };
  // Only a provably unfinished assignment is worth another press: the board
  // recorded that no owner was written. Every other outcome is final here.
  const retryable = created !== null && created.state === "pending";
  const tone =
    created === null || created.state === "assigned"
      ? "text-ok"
      : created.state === "pending" ||
          created.state === "unconfirmed" ||
          created.state === "refused"
        ? "text-warn"
        : "text-ink-200";

  return (
    <div className="rc-proposal" data-kind="ticket-proposal" data-proposal={proposal.id}>
      <div className="rounded border border-ink-700 bg-ink-900 px-3 py-2.5">
        <p className="text-micro text-ink-400">
          {t("New ticket for {name}").replace("{name}", () => proposal.agent.name)}
          {" · "}
          {t("card {id}").replace("{id}", () => proposal.id)}
        </p>
        <p className="text-label text-ink-100 break-words mt-1">{proposal.title}</p>
        <p className="text-secondary text-ink-300 break-words whitespace-pre-wrap mt-1">
          {proposal.brief}
        </p>
        <dl
          className="grid grid-cols-[auto_1fr] gap-x-3 gap-y-1 text-micro mt-2.5"
          aria-label={t("What this assigns")}
        >
          <dt className="text-ink-400">{t("Agent")}</dt>
          <dd className="text-ink-200 break-words">
            {proposal.agent.name} · {t("runs as you")}
          </dd>
          <dt className="text-ink-400">{t("Tools")}</dt>
          <dd className="text-ink-300 break-words font-mono text-micro">
            {proposal.tools.length ? proposal.tools.join(", ") : t("None")}
          </dd>
          <dt className="text-ink-400">{t("Visible to")}</dt>
          <dd className="text-ink-200">{t("Everyone in the company")}</dd>
        </dl>
        {entry.proposalOutcomeUnavailable ? (
          <p className="mt-3 text-label text-warn" role="status">
            {t("Hosted run status is unavailable or not recorded.")}{" "}
            {t("It will be checked again while this conversation is visible.")}
          </p>
        ) : proposal.ticket ? (
          <div className="mt-3" role="status">
            <p className="text-label text-ink-200">
              {t("Created {id}.").replace("{id}", () => proposal.ticket!)}{" "}
              {proposal.run && runLabel && (
                <span className="text-ink-300">
                  {/* The run's own agent: a reassignment can differ from the proposal. */}
                  {proposal.run.agent ? `${proposal.run.agent} ${t(runLabel)}` : t(runLabel)}
                </span>
              )}
            </p>
            {onOpenIssue && (
              <Button variant="ghost" size="sm" onClick={() => onOpenIssue(proposal.ticket!)}>
                {t("Open ticket")}
              </Button>
            )}
          </div>
        ) : created ? (
          <div className="mt-3" role="status">
            <p className={`text-label ${tone}`}>
              {created.state === "assigned" &&
                t("Created {id} and assigned it.").replace("{id}", () => created.ticket)}
              {created.state === "pending" &&
                t("Created {id}. No owner is recorded yet: finish assigning it.")
                  .replace("{id}", () => created.ticket)}
              {created.state === "changed" &&
                (created.owner
                  ? t("Created {id}. Its owner is now {owner}, so it was left as it is.")
                      .replace("{id}", () => created.ticket)
                      .replace("{owner}", () => created.owner!)
                  : t("Created {id}. Its owner was cleared, so it was left as it is.").replace(
                      "{id}",
                      () => created.ticket,
                    ))}
              {created.state === "refused" &&
                t("Created {id}. {name} was not assigned: {reason}.")
                  .replace("{id}", () => created.ticket)
                  .replace("{name}", () => proposal.agent.name)
                  .replace("{reason}", () =>
                    created.reason === "agent_unknown"
                      ? t("the agent profile is not available")
                      : created.reason === "agent_not_permitted"
                        ? t("this board does not allow this assignment")
                        : created.reason === "ticket_changed"
                          ? t("the ticket changed before the assignment was confirmed")
                          : created.reason,
                  )}
              {created.state === "replayed" &&
                t("The card already created {id}; nothing was reassigned.").replace(
                  "{id}",
                  () => created.ticket,
                )}
              {created.state === "owned" &&
                t("{id} already has an owner; nothing was reassigned.").replace(
                  "{id}",
                  () => created.ticket,
                )}
              {created.state === "unconfirmed" &&
                t("Ticket {id} was created. Whether the agent was assigned is not confirmed: {error}")
                  .replace("{id}", () => created.ticket)
                  .replace("{error}", () => created.error === "untracked_existing"
                    ? t("the existing ticket has no assignment receipt")
                    : created.error ?? t("no answer from the board"))}
            </p>
            <div className="flex flex-wrap items-center gap-2">
              {onOpenIssue && (
                <Button variant="ghost" size="sm" onClick={() => onOpenIssue(created.ticket)}>
                  {t("Open ticket")}
                </Button>
              )}
              {retryable && (
                <Button
                  variant="primary"
                  size="sm"
                  loading={busy}
                  disabled={busy || readOnly}
                  onClick={run}
                  title={
                    readOnly
                      ? t("This board is read-only")
                      : t("Finishes the assignment the board proved is pending")
                  }
                >
                  {t("Finish assigning")}
                </Button>
              )}
            </div>
          </div>
        ) : (
          <div className="flex flex-wrap items-center gap-2 mt-3">
            <Button
              variant="primary"
              size="sm"
              loading={busy}
              disabled={busy || readOnly}
              onClick={run}
              title={
                readOnly
                  ? t("This board is read-only")
                  : t("Creates the ticket and starts the agent once")
              }
            >
              {t("Create and assign")}
            </Button>
            <span className="text-micro text-ink-500">
              {readOnly
                ? t("Read-only: this card cannot start the work.")
                : t("Starts one run. A second click does nothing.")}
            </span>
          </div>
        )}
        {error && (
          <p className="text-micro text-fail mt-2" role="alert">
            {error}
          </p>
        )}
      </div>
    </div>
  );
}
