import { useState } from "react";
import { api, ApiError } from "../../lib/api";
import { useTranslate } from "../../lib/locale";
import { resources } from "../../lib/resources";
import Button from "../../ui/Button";
import { TurnCard, type CardDensity, type CardStatus, type CardTone } from "../../ui/TurnCard";
import type { ThreadEntry } from "../home/thread";
import {
  createAndAssign,
  parseTicketProposal,
  proposalCapabilities,
  type CreateAndAssignOutcome,
} from "./ticketProposal";

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

const RUN_TONE: Record<string, CardTone> = {
  finished: "ok",
  no_report: "warn",
  failed: "fail",
  refused: "fail",
  stopped: "muted",
  parked: "warn",
};

/** Master's start card (AOS-517 / CAD-2838): the whole ticket and destination,
 *  the agent that would run it, the tools that agent gets, and who can read
 *  the result. One click creates and assigns; a second click starts nothing. */
export default function TicketProposalCard({
  entry,
  onOpenIssue,
  readOnly = false,
  density = "full",
}: {
  entry: ThreadEntry;
  onOpenIssue?: (id: string) => void;
  /** A read-only board refuses both writes; do not offer the action. */
  readOnly?: boolean;
  density?: CardDensity;
}) {
  const t = useTranslate();
  const proposal = parseTicketProposal(entry.payload);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [created, setCreated] = useState<CreateAndAssignOutcome | null>(null);
  const [copied, setCopied] = useState(false);

  if (!proposal) {
    return (
      <p className="text-micro text-ink-500" data-kind="ticket-proposal">
        · {entry.text || t("This ticket proposal is unavailable. Ask the assistant to try again.")}
      </p>
    );
  }

  const runLabel = proposal.run ? RUN_LABEL[proposal.run.state] : undefined;
  const run = () => {
    setBusy(true);
    setError(null);
    createAndAssign({ ...api, issue: (id: string) => api.issue(id) }, proposal)
      .then((outcome) => {
        setCreated(outcome);
        void resources.issues.invalidate();
        void resources.issue(outcome.ticket).invalidate();
      })
      .catch((e: ApiError) => setError(e.message ?? String(e)))
      .finally(() => setBusy(false));
  };

  const copyId = () => {
    if (typeof navigator !== "undefined" && navigator.clipboard?.writeText) {
      void navigator.clipboard.writeText(proposal.id).catch(() => {});
    }
    setCopied(true);
    setTimeout(() => setCopied(false), 1400);
  };

  const ticketKey = proposal.ticket ?? created?.ticket;
  const status: CardStatus = proposal.ticket
    ? {
        text: runLabel ? t(runLabel) : t("Assigned"),
        tone: proposal.run ? (RUN_TONE[proposal.run.state] ?? "muted") : "info",
      }
    : created
      ? created.state === "assigned"
        ? { text: t("Assigned"), tone: "info" }
        : created.state === "unconfirmed"
          ? { text: t("Unconfirmed"), tone: "warn" }
          : { text: t("Already created"), tone: "muted" }
      : error
        ? { text: t("Failed"), tone: "fail" }
        : readOnly
          ? { text: t("Read-only"), tone: "muted" }
          : { text: t("Draft"), tone: "warn" };

  const capabilities = proposalCapabilities(proposal.tools);

  const footer = proposal.ticket ? (
    <>
      <span className="turncard-ticket-note" role="status">
        {t("Created {id}.").replace("{id}", () => proposal.ticket!)}{" "}
        {proposal.run && runLabel && (
          <span>
            {/* The run's own agent: a reassignment can differ from the proposal. */}
            {proposal.run.agent ? `${proposal.run.agent} ${t(runLabel)}` : t(runLabel)}
          </span>
        )}
      </span>
      {onOpenIssue && (
        <Button variant="ghost" size="sm" onClick={() => onOpenIssue(proposal.ticket!)}>
          {t("Open ticket")}
        </Button>
      )}
    </>
  ) : created ? (
    <>
      <span
        className={`turncard-ticket-note ${created.state === "unconfirmed" ? "text-warn" : "text-ok"}`}
        role="status"
      >
        {created.state === "assigned" &&
          t("Created {id} and assigned it.").replace("{id}", () => created.ticket)}
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
            .replace("{error}", () => created.error ?? t("no answer from the board"))}
      </span>
      {onOpenIssue && (
        <Button variant="ghost" size="sm" onClick={() => onOpenIssue(created.ticket)}>
          {t("Open ticket")}
        </Button>
      )}
    </>
  ) : (
    <>
      <span className="turncard-ticket-note">
        {readOnly
          ? t("Read-only board: cannot start runs")
          : t("Starts one run when approved")}
      </span>
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
    </>
  );

  return (
    <TurnCard
      kind="ticket-proposal"
      density={density}
      className="card turncard-ticket"
      data-kind="ticket-proposal"
      data-proposal={proposal.id}
      label={`${t("Ticket proposal")}: ${proposal.title}`}
      icon={
        <svg viewBox="0 0 16 16" fill="none" stroke="currentColor" strokeWidth="1.4" strokeLinecap="round" strokeLinejoin="round" aria-hidden="true">
          <path d="M2.5 4.5a1 1 0 0 1 1-1h9a1 1 0 0 1 1 1v2a1.25 1.25 0 0 0 0 2.5v2a1 1 0 0 1-1 1h-9a1 1 0 0 1-1-1v-2a1.25 1.25 0 0 0 0-2.5v-2z" />
        </svg>
      }
      kindLabel={
        <>
          <span>{t("Ticket proposal")}</span>
          {ticketKey ? (
            <span className="num text-ink-400">{ticketKey}</span>
          ) : (
            <button
              type="button"
              className="turncard-id-btn"
              title={`${t("Proposal ID")}: ${proposal.id}`}
              aria-label={copied ? t("Copied") : `${t("Copy ID")}: ${proposal.id}`}
              onClick={copyId}
            >
              {copied ? t("Copied") : `#${proposal.id.slice(0, 8)}`}
            </button>
          )}
        </>
      }
      status={status}
      footer={footer}
    >
      <p className="turncard-title">{proposal.title}</p>
      <p className="text-secondary text-ink-300 break-words whitespace-pre-wrap">{proposal.brief}</p>
      <dl className="turncard-fields" aria-label={t("What this assigns")}>
        <div className="turncard-field">
          <dt>{t("Project")}</dt>
          <dd>
            <bdi>{proposal.project}</bdi>{" "}
            <span className="text-ink-400">· {t("Everyone in the company")}</span>
          </dd>
        </div>
        <div className="turncard-field">
          <dt>{t("Assignee")}</dt>
          <dd className="turncard-assignee-row">
            <span className="font-medium text-ink-100">
              <bdi>{proposal.agent.name}</bdi>
            </span>
            <span className="turncard-perm-note">{t("uses your permissions")}</span>
          </dd>
        </div>
        {!proposal.ticket && !created && (
          <div className="turncard-field">
            <dt>{t("Capabilities")}</dt>
            <dd>
              {proposal.tools.length === 0 ? (
                t("None")
              ) : (
                <>
                  <div className="turncard-cap-chips">
                    {capabilities.map((group) => (
                      <span
                        key={group.id}
                        className="turncard-cap-chip"
                        data-optin={group.optIn || undefined}
                      >
                        {t(group.label)}
                      </span>
                    ))}
                  </div>
                  <details className="turncard-receipt">
                    <summary>
                      <span className="turncard-receipt-toggle">
                        <span className="turncard-receipt-caret" aria-hidden="true">▸</span>
                        <span className="turncard-receipt-show">
                          {t("Show {n} tools").replace("{n}", () => String(proposal.tools.length))}
                        </span>
                        <span className="turncard-receipt-hide">
                          {t("Hide {n} tools").replace("{n}", () => String(proposal.tools.length))}
                        </span>
                      </span>
                    </summary>
                    <div className="turncard-receipt-details">
                      <dl className="turncard-fields">
                        {capabilities.map((group) => (
                          <div key={group.id} className="turncard-field">
                            <dt>{t(group.label)}</dt>
                            <dd className="font-mono text-micro text-ink-300">{group.tools.join(", ")}</dd>
                          </div>
                        ))}
                      </dl>
                    </div>
                  </details>
                </>
              )}
            </dd>
          </div>
        )}
      </dl>
      {error && (
        <p className="text-micro text-fail" role="alert">
          {error}
        </p>
      )}
    </TurnCard>
  );
}
