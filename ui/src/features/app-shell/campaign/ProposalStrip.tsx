import { useState } from "react";
import Button from "../../../ui/Button";
import { contentClient } from "../contentClient";
import type { AudienceScope } from "../audienceClient";
import {
  friendlyCampaignError,
  parseContentDoc,
  type ContentDoc,
  type ProposalDoc,
} from "../campaignGrammar";

/**
 * The assistant's pending proposal as a strip above the email preview
 * (CAD-1055). Same safety rules as the old Proposals card, same APIs:
 * - the verified badge needs `actor == "assistant"` AND a non-null
 *   receipt (CAD-813); anything else reads operator-submitted;
 * - Apply is pinned to the saved revision and disabled when the
 *   proposal's stamped source drifted behind it (stale);
 * - Discard never mutates the saved version.
 * The saved/draft toggle only chooses which host render the stage
 * shows — it never saves.
 */
export default function ProposalStrip({
  scope,
  proposal,
  expectedRevision,
  canWrite,
  viewingDraft,
  onViewDraft,
  onApplied,
  onDiscarded,
  onError,
}: {
  scope: AudienceScope;
  proposal: ProposalDoc;
  expectedRevision: number;
  canWrite: boolean;
  viewingDraft: boolean;
  onViewDraft: (view: boolean) => void;
  onApplied: (doc: ContentDoc) => void;
  onDiscarded: (proposalId: string) => void;
  onError: (message: string | null) => void;
}) {
  const [pending, setPending] = useState<"apply" | "discard" | null>(null);
  const receipt = proposal.assistantReceipt;
  const verified = proposal.actor === "assistant" && receipt !== null;
  const stale = proposal.sourceRevision !== expectedRevision;
  const savedLabel = expectedRevision === 0 ? "Nothing saved" : `r${expectedRevision} saved`;

  return (
    <div className="grid gap-1" data-proposal={proposal.proposalId}>
      <div className="crm-strip">
        {verified && receipt !== null ? (
          <span
            className="chip"
            data-badge="verified-assistant"
            title="Host-verified: the assistant produced this draft on this campaign — the browser's copy is never the authority"
          >
            Verified assistant draft
          </span>
        ) : (
          <span
            className="chip"
            data-badge="operator-submitted"
            title="Submitted through the operator proposal route — no assistant provenance"
          >
            Operator-submitted
          </span>
        )}
        <span className="text-label text-ink-300">
          {proposal.subject}
          {verified && receipt !== null ? (
            <span className="text-ink-500"> · from {receipt.agent}</span>
          ) : null}
        </span>
        <span className="flex-1" />
        <span className="crm-seg" role="group" aria-label="Preview version">
          <button
            type="button"
            aria-pressed={!viewingDraft}
            data-version="saved"
            onClick={() => onViewDraft(false)}
          >
            {savedLabel}
          </button>
          <button
            type="button"
            aria-pressed={viewingDraft}
            data-version="draft"
            data-proposal-preview={proposal.proposalId}
            onClick={() => onViewDraft(true)}
          >
            Draft
          </button>
        </span>
        {canWrite && (
          <>
            <Button
              size="sm"
              variant="primary"
              loading={pending === "apply"}
              disabled={pending !== null || stale}
              title={
                stale
                  ? "Apply is disabled: the proposal's stamped source revision is behind the current draft"
                  : `Apply as a new revision (expects the draft at r${expectedRevision})`
              }
              onClick={() => {
                onError(null);
                setPending("apply");
                // An unsaved campaign applies a source_revision=0 proposal
                // to CREATE revision 1 — nothing to pin, so omit the check.
                void contentClient
                  .proposalApply(
                    scope,
                    proposal.proposalId,
                    expectedRevision === 0 ? undefined : expectedRevision,
                  )
                  .then((value) => onApplied(parseContentDoc(value)))
                  .catch((err: unknown) => onError(friendlyCampaignError(err)))
                  .finally(() => setPending(null));
              }}
            >
              Apply as r{expectedRevision + 1}
            </Button>
            <Button
              size="sm"
              variant="ghost"
              loading={pending === "discard"}
              disabled={pending !== null}
              onClick={() => {
                onError(null);
                setPending("discard");
                void contentClient
                  .proposalDiscard(scope, proposal.proposalId)
                  .then(() => onDiscarded(proposal.proposalId))
                  .catch((err: unknown) => onError(friendlyCampaignError(err)))
                  .finally(() => setPending(null));
              }}
            >
              Discard
            </Button>
          </>
        )}
      </div>
      {stale && (
        <p className="text-label text-warn" data-state="stale">
          Needs review (stale) — drafted against r{proposal.sourceRevision}, the saved version is
          now r{expectedRevision}. Re-review its text before asking the assistant to draft again.
        </p>
      )}
      <details className="crm-diag">
        <summary className="text-micro text-ink-500">Technical details</summary>
        <p className="num text-micro text-ink-500 mt-1">
          {proposal.proposalId} · actor {proposal.actor} · origin {proposal.origin} · source r
          {proposal.sourceRevision}
          {receipt !== null ? ` · receipt campaign ${receipt.campaignId}` : ""}
        </p>
      </details>
    </div>
  );
}
