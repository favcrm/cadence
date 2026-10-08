import { useState } from "react";
import Button from "../../../ui/Button";
import type { AudienceScope } from "../audienceClient";
import { contentClient } from "../contentClient";
import {
  friendlyCampaignError,
  type ProposalDoc,
} from "../campaignGrammar";

/**
 * A host-attributed proposal remains separate until the operator moves
 * its body into the local editor draft. Saving that draft is a separate
 * revision-pinned action; loading a suggestion never mutates saved content.
 */
export default function ProposalStrip({
  scope,
  proposal,
  expectedRevision,
  canWrite,
  replacesHtml,
  viewingDraft,
  canUseInEditor,
  onViewDraft,
  onUseInEditor,
  onDiscarded,
  onError,
}: {
  scope: AudienceScope;
  proposal: ProposalDoc;
  expectedRevision: number;
  canWrite: boolean;
  /** The saved body uses HTML; using this proposal converts the unsaved body to blocks. */
  replacesHtml: boolean;
  viewingDraft: boolean;
  canUseInEditor: boolean;
  onViewDraft: (view: boolean) => void;
  onUseInEditor: () => void;
  onDiscarded: (proposalId: string) => void;
  onError: (message: string | null) => void;
}) {
  const [pending, setPending] = useState<"discard" | null>(null);
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
        <span className="crm-strip-actions">
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
            Review
          </button>
        </span>
        {canWrite && (
          <>
            <Button
              size="sm"
              variant="primary"
              disabled={pending !== null || stale || !viewingDraft || !canUseInEditor}
              title={
                stale
                  ? "This suggestion was based on an older saved revision; review it again before use"
                  : viewingDraft
                    ? "Copy this host-attributed suggestion into the unsaved editor draft"
                    : "Review the suggestion before using it in the editor"
              }
              onClick={onUseInEditor}
            >
              Use in editor
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
        </span>
        <details className="crm-diag crm-strip-diag">
          <summary className="text-micro text-ink-500">Technical details</summary>
          <p className="num text-micro text-ink-500 mt-1">
            {proposal.proposalId} · actor {proposal.actor} · origin {proposal.origin} · source r
            {proposal.sourceRevision}
            {receipt !== null ? ` · receipt campaign ${receipt.campaignId}` : ""}
          </p>
        </details>
      </div>
      {replacesHtml && canWrite && (
        <p className="text-label text-warn" data-state="replaces-html">
          Using this suggestion changes the unsaved draft body format; it does not save until you choose Save.
        </p>
      )}
      {stale && (
        <p className="text-label text-warn" data-state="stale">
          Needs review (stale) — drafted against r{proposal.sourceRevision}, the saved version is
          now r{expectedRevision}. Re-review its text before asking the assistant to draft again.
        </p>
      )}
    </div>
  );
}
