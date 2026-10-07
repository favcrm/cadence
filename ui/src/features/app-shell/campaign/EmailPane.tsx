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
import ConfirmDialog from "../shared/ConfirmDialog";
import { blocksFromProposal, blocksToGrammar, type EmailDraftApi } from "./useEmailDraft";

type Mode = "visual" | "html";
type Device = "desktop" | "mobile";

const MODES: [Mode, string][] = [
  ["visual", "Visual"],
  ["html", "HTML"],
];

/**
 * Email tab: pending assistant proposals and exactly two editing modes.
 * Visual block edits and HTML source edits share one revision-pinned
 * draft. Saving is explicit, and the required host footer is protected.
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
  senderBindings,
  senderBindingId,
  onSenderBindingId,
  senderBindingError,
  dirty,
  proposals,
  proposalsError,
  proposalError,
  proposalNote,
  onRefreshDrafts,
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
  senderBindings: { id: string; name: string; address: string }[];
  senderBindingId: string | null;
  onSenderBindingId: (value: string | null) => void;
  senderBindingError: string | null;
  dirty: boolean;
  /** Pending proposals for this campaign only. */
  proposals: ProposalDoc[];
  proposalsError: string | null;
  proposalError: string | null;
  proposalNote: string | null;
  onRefreshDrafts: () => void;
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
  const [replacementConfirmationId, setReplacementConfirmationId] = useState<string | null>(null);

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
  // A host-attributed suggestion is read-only until Use in editor. The
  // local editor is available at revision 0 as well as on saved revisions.
  const editing = canWrite && edit !== null && draft === null;
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
  const blockSource = edit === null
    ? ""
    : blocksToGrammar(edit.draft.blocks)
        .map((block) => {
          const text = block.type === "button" ? block.label : block.text;
          const escaped = text
            .replaceAll("&", "&amp;")
            .replaceAll("<", "&lt;")
            .replaceAll(">", "&gt;");
          if (block.type === "heading") return `<h2>${escaped}</h2>`;
          if (block.type === "button") {
            const url = block.url.replaceAll("&", "&amp;").replaceAll('"', "&quot;");
            return `<p><a href="${url}">${escaped}</a></p>`;
          }
          return `<p>${escaped}</p>`;
        })
        .join("\n");
  const htmlSource = edit?.draft.html ?? blockSource;
  const draftHtmlPreview = (html: string) => {
    const policy = `<meta http-equiv="Content-Security-Policy" content="default-src 'none'; img-src data:; style-src 'unsafe-inline'; font-src 'none'; form-action 'none'; base-uri 'none'">`;
    return /<head(?:\s[^>]*)?>/i.test(html)
      ? html.replace(/<head(?:\s[^>]*)?>/i, (head) => `${head}${policy}`)
      : `<!doctype html><html><head>${policy}</head><body>${html}</body></html>`;
  };

  const useProposalInEditor = (row: ProposalDoc, confirmed = false) => {
    if (
      edit === null ||
      edit.saving ||
      edit.reloading ||
      row.sourceRevision !== expectedRevision ||
      draftId !== row.proposalId ||
      draftRender === null ||
      draftPending ||
      draftError !== null
    ) return;
    if (edit.dirty && !confirmed) {
      setReplacementConfirmationId(row.proposalId);
      return;
    }
    if (confirmed && replacementConfirmationId !== row.proposalId) return;

    setReplacementConfirmationId(null);
    edit.patch({
      subject: row.subject,
      preheader: row.preheader,
      blocks: blocksFromProposal(row.blocks),
      html: null,
      ownText: false,
      text: "",
    });
    setDraftId(null);
  };

  const stage = (() => {
    if (draft === null && doc === null && !editing) {
      return (
        <p className="text-label text-ink-400" data-preview="unsaved">
          No saved email yet. Ask the assistant in the left chat to draft this campaign&apos;s
          email, then use a verified proposal in the editor and explicitly Save revision 1.
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
                    disabled={edit.saving || edit.reloading}
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
                      disabled={edit.saving || edit.reloading}
                    />
                  </label>
                ) : null}
                <button
                  type="button"
                  id="cmp-preheader-toggle"
                  className="lnk crm-preheader-toggle"
                  aria-expanded={preheaderOpen}
                  aria-controls="cmp-preheader"
                  disabled={edit.saving || edit.reloading}
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
                disabled={edit.saving || edit.reloading}
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
          {editing && edit !== null && mode === "html" && (
            <div className="grid gap-2 mt-2">
              <p
                className="card border-warn/40 bg-warn/10 px-3 py-2 text-label text-warn"
                role="note"
                data-html-note
                data-html-save-boundary
              >
                <strong>Save keeps a sanitized body fragment, not a full document.</strong> The host
                removes the doctype and document wrappers (&lt;html&gt;, &lt;head&gt; and &lt;body&gt;),
                including head content such as the title and stylesheet &lt;style&gt; blocks. Only
                selected supported inline style attributes may persist; unsupported or unsafe
                content is stripped. The protected sender and unsubscribe footer are appended
                separately by the host. Switching modes preserves your unsaved source, but Save
                does not save a full document or stylesheet verbatim.
              </p>
              <label className="text-label text-ink-300" htmlFor="cmp-html-source">
                HTML source
              </label>
              <textarea
                id="cmp-html-source"
                className="field srcedit"
                rows={12}
                spellCheck={false}
                value={htmlSource}
                placeholder="Paste or edit the email body HTML"
                disabled={edit.saving || edit.reloading}
                onChange={(e) => edit.patch({ html: e.target.value })}
              />
              <iframe
                title="Live HTML draft preview"
                sandbox=""
                srcDoc={draftHtmlPreview(htmlSource)}
                className="crm-preview-frame"
                data-preview="draft-html"
              />
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
          {editing && mode === "visual" && edit !== null && edit.draft.html !== null && (
            <iframe
              title="Visual preview of HTML draft"
              sandbox=""
              srcDoc={draftHtmlPreview(edit.draft.html)}
              className="crm-preview-frame"
              data-preview="visual-draft-html"
            />
          )}
          {shown !== null && mode === "visual" && (edit === null || edit.draft.html === null) && (
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
          {shown !== null && mode === "html" && !editing && (
            <pre className="crm-preview" data-preview="html">
              {shown.html}
            </pre>
          )}
          {mode === "html" && (
            <details className="crm-diag" data-advanced-text>
              <summary className="text-label text-ink-300">Advanced: plain-text version</summary>
              <div className="grid gap-2 mt-2">
                {editing && edit !== null && (
                  <>
                    <label className="text-label text-ink-300" htmlFor="cmp-own-text">
                      <input
                        id="cmp-own-text"
                        type="checkbox"
                        checked={edit.draft.ownText}
                        disabled={edit.saving || edit.reloading}
                        onChange={(event) =>
                          edit.patch(
                            event.target.checked
                              ? {
                                  ownText: true,
                                  text: edit.draft.text === "" ? (render?.text ?? "") : edit.draft.text,
                                }
                              : { ownText: false, text: "" },
                          )
                        }
                      />{" "}
                      Write my own
                    </label>
                    {edit.draft.ownText ? (
                      <div className="crm-field">
                        <label className="text-label text-ink-300" htmlFor="cmp-text-override">
                          Custom plain-text override
                        </label>
                        <textarea
                          id="cmp-text-override"
                          className="field"
                          rows={8}
                          value={edit.draft.text}
                          disabled={edit.saving || edit.reloading}
                          onChange={(event) => edit.patch({ text: event.target.value })}
                        />
                        <p className="text-micro text-ink-500">
                          Unsaved custom text is saved only when you choose Save.
                        </p>
                      </div>
                    ) : (
                      <p className="text-micro text-ink-500">
                        The host generates plain text from the email body unless you write an override.
                      </p>
                    )}
                  </>
                )}
                {shown !== null && (
                  <div className="crm-field" data-preview="plain-text">
                    <span className="text-label text-ink-300">
                      {draft !== null
                        ? "Suggested plain-text render (not saved)"
                        : doc?.textOverride != null
                          ? `Saved custom plain-text render (v${doc.revision})`
                          : doc !== null
                            ? `Saved host-generated plain text (v${doc.revision})`
                            : "Host-rendered plain text"}
                    </span>
                    <pre className="crm-preview">{shown.text}</pre>
                  </div>
                )}
                <p className="text-micro text-ink-500" data-host-footer-note>
                  The required sender and unsubscribe footer is appended by the host and cannot be edited here.
                </p>
              </div>
            </details>
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
          <div key={row.proposalId}>
            <ProposalStrip
              scope={scope}
              proposal={row}
              expectedRevision={expectedRevision}
              canWrite={canWrite}
              replacesHtml={doc?.mode === "html"}
              viewingDraft={draftId === row.proposalId}
              canUseInEditor={
                draftId === row.proposalId &&
                draftRender !== null &&
                !draftPending &&
                draftError === null &&
                edit !== null &&
                !edit.saving &&
                !edit.reloading
              }
              onViewDraft={(view) => {
                setReplacementConfirmationId(null);
                setDraftId(view ? row.proposalId : null);
              }}
              onUseInEditor={() => useProposalInEditor(row)}
              onDiscarded={onDiscarded}
              onError={onProposalError}
            />
            {replacementConfirmationId === row.proposalId && edit?.dirty && (
              <ConfirmDialog
                title="Replace unsaved email changes?"
                body={
                  <p>
                    Using this proposal replaces your unsaved subject, preheader, body, and text override.
                    Saved content stays unchanged until you choose Save.
                  </p>
                }
                confirmLabel="Replace local draft"
                pending={edit.saving || edit.reloading}
                error={null}
                onConfirm={() => useProposalInEditor(row, true)}
                onCancel={() => setReplacementConfirmationId(null)}
              />
            )}
          </div>
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
            Use in editor changes only the local draft. Save creates a new revision; Discard changes nothing.{" "}
            <button type="button" className="lnk" onClick={onRefreshDrafts}>
              Refresh drafts
            </button>
          </p>
        )}
      </section>

      <section aria-label="Email preview" className="grid gap-3">
        <div className="crm-pvbar">
          <span className="crm-seg" role="group" aria-label="Email editing mode">
            {MODES.map(([key, label]) => (
              <button key={key} type="button" aria-pressed={mode === key} onClick={() => setMode(key)}>
                {label}
              </button>
            ))}
          </span>
          {editingBlocks && edit !== null && (
            <AddBlockTools
              disabled={edit.saving || edit.reloading || edit.draft.blocks.length >= MAX_BLOCKS}
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
            <label className="text-label text-ink-300" htmlFor="cmp-sender-preview">
              Sender preview (not send authorization)
            </label>
            <select
              id="cmp-sender-preview"
              className="field"
              disabled={!canWrite}
              value={senderBindingId ?? ""}
              onChange={(event) => onSenderBindingId(event.target.value === "" ? null : event.target.value)}
            >
              <option value="">Preview placeholder — no sender selected</option>
              {senderBindings.map((binding) => (
                <option key={binding.id} value={binding.id}>{binding.name} · {binding.address}</option>
              ))}
            </select>
          </div>
          {senderBindingError !== null && <p className="text-label text-fail" role="alert">{senderBindingError}</p>}
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
        {editing && edit !== null && mode === "visual" && edit.draft.html !== null && (
          <p
            className="card border-warn/40 bg-warn/10 px-3 py-2 text-label text-warn"
            role="note"
            data-state="html-body"
            data-html-save-boundary
          >
            <strong>Save keeps a sanitized body fragment, not this full document.</strong> The host
            removes the doctype and document wrappers (&lt;html&gt;, &lt;head&gt; and &lt;body&gt;),
            including head content such as the title and stylesheet &lt;style&gt; blocks. Only
            selected supported inline style attributes may persist; unsupported or unsafe content
            is stripped. The protected sender and unsubscribe footer are appended separately by
            the host. Switching modes preserves your unsaved source, but Save does not save a full
            document or stylesheet verbatim.
          </p>
        )}
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
            Review this suggestion and use it in the editor; nothing is saved until you choose Save.
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
                <Button size="sm" variant="ghost" loading={edit.reloading} disabled={edit.saving || edit.reloading} onClick={edit.discard}>
                  {edit.reloading ? "Reloading latest…" : "Discard"}
                </Button>
                <Button size="sm" variant="primary" loading={edit.saving} disabled={edit.saving || edit.reloading} onClick={edit.save}>
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
