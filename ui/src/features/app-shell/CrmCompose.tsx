import { useState } from "react";
import { ApiError } from "../../lib/api";
import { contentClient, type ContentBlock, type ContentScope } from "./contentClient";

/**
 * Campaign email composer (CAD-782). Independent of CAD-781's
 * customer screens and CAD-784's editor/chat wiring: this file
 * touches no shared outlet, shell or record-client file.
 *
 * The operator drafts subject/preheader and bounded heading,
 * paragraph and button blocks; a sample name previews the approved
 * first-name token; HTML/text tabs render the exact saved revision
 * with sender material from the named binding (preview-only
 * placeholders when unnamed, bannered as such — never send-ready).
 * A submitted draft offers explicit Apply (new revision, approval
 * invalidated) or Discard (no change); submissions are recorded as
 * operator work, and the assistant never edits silently.
 *
 * MOUNT CONTRACT (CAD-784 owns visible wiring): the campaign route
 * renders `<CrmCompose scope campaignId />` beside the persistent
 * left chat.
 */

export default function CrmCompose({
  scope,
  campaignId,
}: {
  scope: ContentScope;
  campaignId: string;
}) {
  const [subject, setSubject] = useState("");
  const [preheader, setPreheader] = useState("");
  const [blocksText, setBlocksText] = useState("[]");
  const [sampleName, setSampleName] = useState("");
  const [bindingId, setBindingId] = useState("");
  const [previewTab, setPreviewTab] = useState<"html" | "text">("html");
  const [preview, setPreview] = useState("");
  const [previewOnly, setPreviewOnly] = useState(true);
  const [proposal, setProposal] = useState<null | {
    proposal_id: string;
    subject: string;
    state: string;
  }>(null);
  const [notice, setNotice] = useState("");
  const [revision, setRevision] = useState<number | null>(null);

  function parseBlocks(): ContentBlock[] {
    const raw = JSON.parse(blocksText) as unknown;
    if (!Array.isArray(raw)) throw new ApiError("blocks must be an array", 400);
    return raw as ContentBlock[];
  }

  async function run(action: () => Promise<unknown>, ok: (value: any) => void) {
    try {
      setNotice("");
      ok(await action());
    } catch (error) {
      setNotice(error instanceof ApiError ? error.message : "request refused");
    }
  }

  return (
    <div className="app-outlet" data-outlet="compose">
      <section aria-label="Email content">
        <h2 tabIndex={-1}>Campaign content</h2>
        <label>
          Subject
          <input value={subject} onChange={(e) => setSubject(e.target.value)} maxLength={150} />
        </label>
        <label>
          Preheader
          <input value={preheader} onChange={(e) => setPreheader(e.target.value)} maxLength={200} />
        </label>
        <label>
          Blocks (heading / paragraph / button JSON)
          <textarea
            value={blocksText}
            onChange={(e) => setBlocksText(e.target.value)}
            rows={6}
          />
        </label>
        <div>
          <button
            type="button"
            onClick={() =>
              run(
                () =>
                  contentClient.save(scope, {
                    campaignId,
                    subject,
                    preheader,
                    blocks: parseBlocks(),
                    ...(revision === null ? {} : { expectedRevision: revision }),
                  }),
                (value) => {
                  setRevision(value.content.revision as number);
                  setNotice(`Saved revision ${value.content.revision}`);
                },
              )
            }
          >
            Save revision
          </button>
          <button
            type="button"
            onClick={() =>
              run(
                () =>
                  contentClient.render(scope, campaignId, {
                    ...(sampleName ? { sampleFirstName: sampleName } : {}),
                    ...(bindingId ? { bindingId } : {}),
                  }),
                (value) => {
                  setPreview(
                    previewTab === "html" ? value.render.html : value.render.text,
                  );
                  setRevision(value.render.revision as number);
                  setPreviewOnly(value.render.preview_only as boolean);
                },
              )
            }
          >
            Preview
          </button>
        </div>
        <label>
          Sender binding (verified sender; empty previews placeholders)
          <input
            value={bindingId}
            onChange={(e) => setBindingId(e.target.value)}
            maxLength={64}
          />
        </label>
        {previewOnly && (
          <p role="note">
            Preview-only sender placeholders — final-send preparation refuses them.
          </p>
        )}
        <label>
          Sample first name
          <input
            value={sampleName}
            onChange={(e) => setSampleName(e.target.value)}
            maxLength={40}
          />
        </label>
        <div role="tablist" aria-label="Preview format">
          <button
            type="button"
            role="tab"
            aria-selected={previewTab === "html"}
            onClick={() => setPreviewTab("html")}
          >
            HTML
          </button>
          <button
            type="button"
            role="tab"
            aria-selected={previewTab === "text"}
            onClick={() => setPreviewTab("text")}
          >
            Text
          </button>
        </div>
        <pre data-preview={previewTab}>{preview}</pre>
      </section>

      {proposal && (
        <section aria-label="Submitted proposal">
          <h3>Submitted proposal</h3>
          <p>
            {proposal.proposal_id} · {proposal.subject} · {proposal.state} · submitted by the
            operator, bound to its source revision.
          </p>
          <button
            type="button"
            onClick={() =>
              run(
                () =>
                  contentClient.proposalApply(
                    scope,
                    proposal.proposal_id,
                    revision === null ? undefined : revision,
                  ),
                (value) => {
                  setRevision(value.content.revision as number);
                  setProposal(null);
                  setNotice(`Applied as revision ${value.content.revision}`);
                },
              )
            }
          >
            Apply
          </button>
          <button
            type="button"
            onClick={() =>
              run(() => contentClient.proposalDiscard(scope, proposal.proposal_id), () => {
                setProposal(null);
                setNotice("Proposal discarded — content unchanged");
              })
            }
          >
            Discard
          </button>
        </section>
      )}

      {notice && (
        <p role="status">{notice}</p>
      )}
      <p className="num text-micro">
        Installation {scope.installId} · Context {scope.contextId} · Campaign {campaignId}
        {revision === null ? "" : ` · Revision ${revision}`}
      </p>
    </div>
  );
}
