import { useEffect, useState, type ReactNode } from "react";
import Button from "../../../ui/Button";
import type { AudienceScope } from "../audienceClient";
import {
  friendlyCampaignError,
  parseProposalRender,
  type ContentDoc,
  type ContentRender,
  type ProposalDoc,
  type ProposalRenderDoc,
} from "../campaignGrammar";
import { contentClient } from "../contentClient";
import ProposalStrip from "./ProposalStrip";

type Mode = "visual" | "html" | "text";
type Device = "desktop" | "mobile";

const MODES: [Mode, string][] = [
  ["visual", "Visual"],
  ["html", "HTML"],
  ["text", "Text"],
];

/**
 * Email tab (CAD-1055, view only): pending assistant proposal strip,
 * preview toolbar (Visual/HTML/Text, Desktop/Mobile, sample
 * recipient), an envelope header and the host-rendered stage. The
 * stage only ever shows host-rendered, preview-only bytes — saved
 * revision or the proposal's own inert draft; unsaved editor text is
 * never rendered. `editor` (the existing inline subject/text
 * correction) replaces the stage while open.
 */
export default function EmailPane({
  scope,
  canWrite,
  doc,
  render,
  renderPending,
  renderError,
  onRefresh,
  sampleName,
  onSampleName,
  dirty,
  proposals,
  proposalsError,
  proposalError,
  proposalNote,
  onRefreshDrafts,
  onApplied,
  onDiscarded,
  onProposalError,
  editor,
  editAction,
}: {
  scope: AudienceScope;
  canWrite: boolean;
  doc: ContentDoc | null;
  render: ContentRender | null;
  renderPending: boolean;
  renderError: string | null;
  onRefresh: () => void;
  sampleName: string;
  onSampleName: (value: string) => void;
  dirty: boolean;
  /** Pending proposals for this campaign only. */
  proposals: ProposalDoc[];
  proposalsError: string | null;
  proposalError: string | null;
  proposalNote: string | null;
  onRefreshDrafts: () => void;
  onApplied: (doc: ContentDoc) => void;
  onDiscarded: (proposalId: string) => void;
  onProposalError: (message: string | null) => void;
  editor: ReactNode | null;
  editAction: ReactNode;
}) {
  const [mode, setMode] = useState<Mode>("visual");
  const [device, setDevice] = useState<Device>("desktop");
  const [draftId, setDraftId] = useState<string | null>(null);
  const [draftRender, setDraftRender] = useState<ProposalRenderDoc | null>(null);
  const [draftPending, setDraftPending] = useState(false);
  const [draftError, setDraftError] = useState<string | null>(null);

  const draft = draftId === null ? null : (proposals.find((p) => p.proposalId === draftId) ?? null);
  // A decided/applied proposal leaves the list: fall back to the saved view.
  useEffect(() => {
    if (draftId !== null && draft === null) setDraftId(null);
  }, [draftId, draft]);

  useEffect(() => {
    if (draft === null) {
      setDraftRender(null);
      setDraftError(null);
      return;
    }
    const controller = new AbortController();
    setDraftRender(null);
    setDraftPending(true);
    setDraftError(null);
    contentClient
      .proposalRender(scope, draft.proposalId)
      .then((value) => {
        if (!controller.signal.aborted) setDraftRender(parseProposalRender(value));
      })
      .catch((e: unknown) => {
        if (!controller.signal.aborted) setDraftError(friendlyCampaignError(e));
      })
      .finally(() => {
        if (!controller.signal.aborted) setDraftPending(false);
      });
    return () => controller.abort();
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [scope.installId, scope.contextId, draftId]);

  const shown = draft !== null ? draftRender : render;
  const subject = draft !== null ? draft.subject : (doc?.subject ?? "");
  const preheader = draft !== null ? draft.preheader : (doc?.preheader ?? "");
  const expectedRevision = doc === null ? 0 : doc.revision;

  const stage = (() => {
    if (draft === null && doc === null) {
      return (
        <p className="text-label text-ink-400" data-preview="unsaved">
          No saved email yet. Ask the assistant in the left chat to draft this campaign&apos;s
          email, then Apply its verified proposal above to create revision 1.
        </p>
      );
    }
    return (
      <div className="crm-stage">
        <div
          className="crm-inbox"
          data-device={device}
          {...(draft !== null ? { "data-proposal-body": draft.proposalId } : {})}
        >
          <div className="crm-envelope" aria-label="Email envelope">
            <p>
              <span className="text-ink-500">Subject </span>
              <strong className="text-ink-100">{subject}</strong>
            </p>
            <p>
              <span className="text-ink-500">Preheader </span>
              {preheader === "" ? "—" : preheader}
            </p>
            <p className="text-label text-ink-400">
              From{" "}
              <span className="num">
                {shown !== null ? `${shown.sender.name} <${shown.sender.address}>` : "…"}
              </span>{" "}
              · To <span className="num">{sampleName.trim() === "" ? "each recipient" : sampleName.trim()}</span>{" "}
              <span className="chip" title="Sender material is host-locked preview-only bytes">
                preview-only
              </span>
            </p>
          </div>
          {shown === null && (draft !== null ? draftPending : renderPending) && (
            <p className="text-label text-ink-500 mt-2" role="status" data-preview="loading">
              {draft !== null ? "Rendering the draft preview…" : "Rendering the saved email…"}
            </p>
          )}
          {shown !== null && mode === "visual" && (
            <iframe
              title={
                draft !== null
                  ? "Draft email preview"
                  : `Visual email preview, saved revision ${render?.revision ?? ""}`
              }
              sandbox=""
              srcDoc={shown.html}
              className="crm-preview-frame"
              data-preview="visual"
            />
          )}
          {shown !== null && mode !== "visual" && (
            <pre className="crm-preview" data-preview={mode}>
              {mode === "html" ? shown.html : shown.text}
            </pre>
          )}
          {draft !== null && draftError !== null && (
            <div className="grid gap-1 mt-2">
              <p className="text-label text-fail" role="alert">
                {draftError}
              </p>
              <ol className="grid gap-1" aria-label="Draft body blocks">
                {draft.blocks.map((block, index) => (
                  <li key={index} className="text-label text-ink-300">
                    {block.type === "heading" ? (
                      <strong className="text-ink-100">{block.text}</strong>
                    ) : block.type === "button" ? (
                      <span className="chip" data-block="button">
                        Button: {block.label}
                      </span>
                    ) : (
                      block.text
                    )}
                  </li>
                ))}
              </ol>
            </div>
          )}
        </div>
      </div>
    );
  })();

  return (
    <div className="grid gap-3">
      <section aria-label="Assistant proposals" className="grid gap-2">
        {proposalsError && (
          <p className="text-label text-fail" role="alert">
            {proposalsError}{" "}
            <button type="button" className="lnk" onClick={onRefreshDrafts}>
              Retry
            </button>
          </p>
        )}
        {proposalError && (
          <p className="text-label text-fail" role="alert">
            {proposalError}
          </p>
        )}
        {proposalNote && (
          <p className="text-label text-ok" role="status">
            {proposalNote}
          </p>
        )}
        {proposals.map((row) => (
          <ProposalStrip
            key={row.proposalId}
            scope={scope}
            proposal={row}
            expectedRevision={expectedRevision}
            canWrite={canWrite}
            viewingDraft={draftId === row.proposalId}
            onViewDraft={(view) => setDraftId(view ? row.proposalId : null)}
            onApplied={onApplied}
            onDiscarded={onDiscarded}
            onError={onProposalError}
          />
        ))}
        {proposals.length === 0 && (
          <p className="text-label text-ink-400" data-empty="proposals">
            No pending assistant draft.
            {canWrite && (
              <span data-assistant-hint>
                {" "}
                Ask the assistant in the left chat to draft or improve this email — its proposal
                appears here for review.{" "}
                <button type="button" className="lnk" onClick={onRefreshDrafts}>
                  Refresh drafts
                </button>
              </span>
            )}
          </p>
        )}
        {proposals.length > 0 && canWrite && (
          <p className="text-micro text-ink-500">
            Only Apply changes the saved version (approval resets); Discard changes nothing.{" "}
            <button type="button" className="lnk" onClick={onRefreshDrafts}>
              Refresh drafts
            </button>
          </p>
        )}
      </section>

      <section aria-label="Email preview" className="grid gap-3">
        <div className="crm-pvbar">
          <span className="crm-seg" role="group" aria-label="Preview format">
            {MODES.map(([key, label]) => (
              <button key={key} type="button" aria-pressed={mode === key} onClick={() => setMode(key)}>
                {label}
              </button>
            ))}
          </span>
          <span className="crm-seg" role="group" aria-label="Preview width">
            {(["desktop", "mobile"] as Device[]).map((key) => (
              <button key={key} type="button" aria-pressed={device === key} data-device={key} onClick={() => setDevice(key)}>
                {key === "desktop" ? "Desktop" : "Mobile"}
              </button>
            ))}
          </span>
          <div className="crm-field">
            <label className="text-label text-ink-300" htmlFor="cmp-sample">
              Sample recipient first name (optional)
            </label>
            <input
              id="cmp-sample"
              className="field"
              value={sampleName}
              onChange={(e) => onSampleName(e.target.value)}
              maxLength={40}
              autoComplete="off"
              placeholder="Ada"
            />
          </div>
          {doc !== null && (
            <Button
              size="sm"
              loading={renderPending}
              disabled={renderPending}
              title="Re-render the last saved version with the current sample name"
              onClick={onRefresh}
            >
              Refresh preview
            </Button>
          )}
        </div>
        {renderError !== null && !renderPending && draft === null && (
          <p className="text-label text-fail" role="alert">
            {renderError}{" "}
            <button type="button" className="lnk" onClick={onRefresh}>
              Retry
            </button>
          </p>
        )}
        {draft !== null ? (
          <p className="text-micro text-ink-500" data-preview="draft-note">
            Showing the assistant&apos;s draft — nothing is saved until you Apply it.
          </p>
        ) : (
          dirty && (
            <p className="text-micro text-ink-500" data-preview="dirty">
              Preview shows the last saved version. Save changes to refresh.
            </p>
          )
        )}
        {editor !== null ? editor : stage}
        {editor === null && editAction}
      </section>
    </div>
  );
}
