import { useEffect, useState } from "react";
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
import EmailBlocksCanvas, { AddBlockTools, MAX_BLOCKS } from "./EmailBlocksCanvas";
import ProposalStrip from "./ProposalStrip";
import type { EmailDraftApi } from "./useEmailDraft";

type Mode = "visual" | "html" | "text";
type Device = "desktop" | "mobile";

const MODES: [Mode, string][] = [
  ["visual", "Visual"],
  ["html", "HTML"],
  ["text", "Text"],
];

/**
 * Email tab: pending assistant proposal strip, preview toolbar
 * (Visual/HTML/Text, Desktop/Mobile, sample recipient), an envelope
 * header and the host-rendered stage. The host render only ever shows
 * preview-only bytes — saved revision or the proposal's own inert
 * draft; unsaved editor text is never rendered as the email.
 * CAD-1057: an operator edits inline — blocks, subject and preheader
 * (Visual), pasted HTML (HTML), an optional plain-text override
 * (Text) — and a sticky bar saves a new version. The host footer is
 * locked. `edit` is null for a read-only viewer.
 * CAD-1146: Visual mode is the approved direct-visual canvas — the
 * supported blocks are edited directly on the email with inline
 * move/delete menus and undo; the preheader starts collapsed and the
 * host render below stays the saved truth.
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
  edit,
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
  edit: EmailDraftApi | null;
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
  // Inline editing is for an operator on a saved email, never while a
  // proposal draft is on the stage.
  const editing = edit !== null && doc !== null && draft === null;
  // CAD-1146: the optional preheader starts collapsed; hiding it never
  // discards its value and never makes it required. A saved preheader
  // opens the row on load so its value is never hidden silently.
  const [preheaderOpen, setPreheaderOpen] = useState((doc?.preheader ?? "") !== "");
  useEffect(() => {
    if ((doc?.preheader ?? "") !== "") setPreheaderOpen(true);
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [doc?.campaignId, doc?.revision]);
  const editingBlocks =
    editing && edit !== null && mode === "visual" && edit.draft.html === null;

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
            {editing && edit !== null ? (
              <>
                <label className="crm-env-row">
                  <span className="text-ink-500">Subject</span>
                  <input
                    id="cmp-subject"
                    className="field crm-subject-direct"
                    value={edit.draft.subject}
                    onChange={(e) => edit.patch({ subject: e.target.value })}
                    maxLength={150}
                    autoComplete="off"
                    disabled={edit.saving}
                  />
                </label>
                {preheaderOpen ? (
                  <label className="crm-env-row">
                    <span className="text-ink-500">Preheader</span>
                    <input
                      id="cmp-preheader"
                      className="field crm-preheader-direct"
                      value={edit.draft.preheader}
                      onChange={(e) => edit.patch({ preheader: e.target.value })}
                      maxLength={200}
                      autoComplete="off"
                      placeholder="Optional inbox preview text"
                      disabled={edit.saving}
                    />
                  </label>
                ) : null}
                <button
                  type="button"
                  id="cmp-preheader-toggle"
                  className="lnk crm-preheader-toggle"
                  aria-expanded={preheaderOpen}
                  aria-controls="cmp-preheader"
                  disabled={edit.saving}
                  onClick={() => setPreheaderOpen((open) => !open)}
                >
                  {preheaderOpen ? "− Hide preheader" : "+ Add preheader"}{" "}
                  <span className="text-ink-500">(optional)</span>
                </button>
              </>
            ) : (
              <>
                <p>
                  <span className="text-ink-500">Subject </span>
                  <strong className="text-ink-100">{subject}</strong>
                </p>
                <p>
                  <span className="text-ink-500">Preheader </span>
                  {preheader === "" ? "—" : preheader}
                </p>
              </>
            )}
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
          {editingBlocks && edit !== null && (
            <>
              <EmailBlocksCanvas
                blocks={edit.draft.blocks}
                disabled={edit.saving}
                brandName={shown !== null ? shown.sender.name : "Email"}
                device={device}
                canUndo={edit.canUndo}
                onUndo={edit.undoBlocks}
                onStructure={(blocks) => {
                  if (blocks.length <= MAX_BLOCKS) edit.setBlocks(blocks);
                }}
                onText={(key, change) =>
                  edit.patch({
                    blocks: edit.draft.blocks.map((block) =>
                      block.key === key ? { ...block, ...change } : block,
                    ),
                  })
                }
              />
              <p className="crm-footer-lock" data-host-footer>
                🔒 host footer — the unsubscribe link and sender address are added by the host
                and cannot be edited.
              </p>
            </>
          )}
          {editing && edit !== null && mode === "visual" && edit.draft.html !== null && (
            <p className="text-label text-ink-400 mt-2" data-state="html-body">
              This email&apos;s body is HTML — edit it in HTML mode. Blocks return only if
              you Discard or Apply an assistant draft.
            </p>
          )}
          {editing && edit !== null && mode === "html" && (
            <div className="grid gap-1 mt-2">
              <label className="text-label text-ink-300" htmlFor="cmp-html-source">
                HTML source
              </label>
              <textarea
                id="cmp-html-source"
                className="field srcedit"
                rows={12}
                spellCheck={false}
                value={edit.draft.html ?? ""}
                placeholder="Paste or edit the email body HTML"
                disabled={edit.saving}
                onChange={(e) =>
                  edit.patch({
                    html: e.target.value === "" && doc?.mode !== "html" ? null : e.target.value,
                  })
                }
              />
              <p className="text-micro text-ink-500" data-html-note>
                The host sanitises this HTML (scripts, forms, event handlers and tracking pixels
                are removed) and adds the unsubscribe footer.
                {edit.draft.html !== null && edit.draft.blocks.length > 0 && doc?.mode !== "html"
                  ? " Saving it replaces the blocks."
                  : ""}
              </p>
            </div>
          )}
          {editing && edit !== null && mode === "text" && (
            <div className="grid gap-1 mt-2">
              <label className="text-label text-ink-300">
                <input
                  type="checkbox"
                  checked={edit.draft.ownText}
                  disabled={edit.saving}
                  onChange={(e) =>
                    edit.patch({
                      ownText: e.target.checked,
                      text: edit.draft.text === "" ? (render?.text ?? "") : edit.draft.text,
                    })
                  }
                />{" "}
                Write my own
              </label>
              {edit.draft.ownText ? (
                <textarea
                  id="cmp-text-override"
                  className="field"
                  rows={8}
                  value={edit.draft.text}
                  disabled={edit.saving}
                  onChange={(e) => edit.patch({ text: e.target.value })}
                />
              ) : (
                <p className="text-micro text-ink-500">
                  The plain-text version is generated from the email body.
                </p>
              )}
            </div>
          )}
          {editing && (
            <p className="text-micro text-ink-500 mt-2">
              Host render of the saved version{doc !== null ? ` (v${doc.revision})` : ""}:
            </p>
          )}
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
            replacesHtml={doc?.mode === "html"}
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
          {editingBlocks && edit !== null && (
            <AddBlockTools
              disabled={edit.saving || edit.draft.blocks.length >= MAX_BLOCKS}
              full
              onAdd={(block) => {
                if (edit.draft.blocks.length < MAX_BLOCKS) {
                  edit.setBlocks([...edit.draft.blocks, block]);
                }
              }}
            />
          )}
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
        {stage}
        {doc !== null && !canWrite && (
          <p className="text-label text-ink-400" data-state="read-only">
            Read-only view. A verified operator saves content revisions.
          </p>
        )}
        {editing && edit !== null && (
          <>
            {edit.note !== null && !edit.dirty && (
              <p className="text-label text-ok" role="status" data-saved-note>
                {edit.note}
              </p>
            )}
            {edit.dirty && (
              <div className="crm-unsaved" data-unsaved-bar>
                <div className="grid gap-1">
                  <span className="text-label text-ink-100">
                    Unsaved changes to v{edit.source} · Saving creates v{edit.source + 1} and
                    resets approval
                  </span>
                  {edit.stale && (
                    <span className="text-micro text-warn" role="status" data-stale-edit>
                      A newer version was saved while you were editing — your text is kept, but
                      saving is pinned to v{edit.source} and will be refused rather than
                      overwrite it.
                    </span>
                  )}
                  {edit.error !== null && (
                    <span className="text-micro text-fail" role="alert">
                      {edit.error}
                    </span>
                  )}
                </div>
                <span className="flex-1" />
                <Button size="sm" variant="ghost" disabled={edit.saving} onClick={edit.discard}>
                  Discard
                </Button>
                <Button size="sm" variant="primary" loading={edit.saving} disabled={edit.saving} onClick={edit.save}>
                  {`Save as v${edit.source + 1}`}
                </Button>
              </div>
            )}
            {!edit.dirty && edit.error !== null && (
              <p className="text-label text-fail" role="alert">
                {edit.error}
              </p>
            )}
          </>
        )}
      </section>
    </div>
  );
}
