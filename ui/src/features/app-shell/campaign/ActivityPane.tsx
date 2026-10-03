import type { ReactNode } from "react";
import type { ContentDoc, ContentRender, ProposalDoc } from "../campaignGrammar";
import type { SmtpBinding } from "../sendClient";

/**
 * Activity tab (CAD-1055): a plain-language revision/proposal timeline
 * built from what the host reported, the sends history, and every id,
 * digest, binding and unsubscribe-origin detail behind one Technical
 * details disclosure. The host wire carries no timestamps for these
 * rows, so the timeline is ordered by state, never by an invented time.
 */
export default function ActivityPane({
  campaignId,
  contextId,
  doc,
  proposals,
  render,
  binding,
  sends,
}: {
  campaignId: string;
  contextId: string;
  doc: ContentDoc | null;
  proposals: ProposalDoc[];
  render: ContentRender | null;
  binding: SmtpBinding | null | undefined;
  /** The sends-history panel. */
  sends: ReactNode;
}) {
  const pending = proposals.filter((row) => row.state === "pending");
  const decided = proposals.filter((row) => row.state !== "pending");
  const approved = doc !== null && doc.approval.valid && doc.approval.revision === doc.revision;
  const who = (row: ProposalDoc) =>
    row.actor === "assistant" && row.assistantReceipt !== null ? "Assistant" : "An operator";
  return (
    <>
      <section aria-label="Campaign timeline" className="card px-4 py-4 grid gap-3">
        <h4 className="text-cardtitle font-medium text-ink-100">Timeline</h4>
        <ol className="crm-timeline" data-timeline>
          {pending.map((row) => (
            <li key={row.proposalId} data-event="proposal-pending" data-accent>
              <span className="crm-dot" aria-hidden="true" />
              <span>
                {who(row)} drafted “{row.subject}” — waiting for your review
              </span>
            </li>
          ))}
          {doc !== null && (
            <li data-event="approval">
              <span className="crm-dot" aria-hidden="true" />
              <span>{approved ? `Content approved at r${doc.revision}` : `r${doc.revision} is not approved`}</span>
            </li>
          )}
          {doc !== null && (
            <li data-event="revision">
              <span className="crm-dot" aria-hidden="true" />
              <span>
                Latest saved version is r{doc.revision}: “{doc.subject}”
              </span>
            </li>
          )}
          {decided.map((row) => (
            <li key={row.proposalId} data-event={`proposal-${row.state}`}>
              <span className="crm-dot" aria-hidden="true" />
              <span>
                {who(row)}&apos;s draft “{row.subject}” was {row.state}
              </span>
            </li>
          ))}
          {doc === null && pending.length === 0 && decided.length === 0 && (
            <li data-event="empty">
              <span className="crm-dot" aria-hidden="true" />
              <span>No versions or drafts yet.</span>
            </li>
          )}
        </ol>
        <details className="crm-diag" data-technical>
          <summary className="text-micro text-ink-500">Technical details</summary>
          <dl className="sdetail mt-1">
            <div>
              <dt>Campaign id</dt>
              <dd className="num">{campaignId}</dd>
            </div>
            <div>
              <dt>Workspace</dt>
              <dd className="num">{contextId || "none"}</dd>
            </div>
            {doc !== null && (
              <>
                <div>
                  <dt>Revision</dt>
                  <dd className="num">
                    r{doc.revision} · {doc.contentDigest}
                  </dd>
                </div>
                <div>
                  <dt>Approval</dt>
                  <dd className="num">
                    {doc.approval.valid
                      ? `r${doc.approval.revision} · ${doc.approval.digest ?? "—"} (${doc.approval.scope})`
                      : "none on this revision"}
                  </dd>
                </div>
              </>
            )}
            {render !== null && (
              <>
                <div>
                  <dt>Preview binding</dt>
                  <dd className="num">
                    {render.bindingId} · preview-only
                    {render.previewOnly ? "" : " (unexpected — reported)"}
                  </dd>
                </div>
                <div>
                  <dt>Unsubscribe link</dt>
                  <dd className="num">{render.unsubscribeUrl}</dd>
                </div>
              </>
            )}
            {binding != null && (
              <div>
                <dt>Sender link</dt>
                <dd className="num">
                  {binding.connectionId} · link r{binding.linkRevision} · {binding.digest}
                </dd>
              </div>
            )}
            {proposals.map((row) => (
              <div key={row.proposalId}>
                <dt>Proposal</dt>
                <dd className="num">
                  {row.proposalId} · {row.state} · actor {row.actor} · origin {row.origin} · source r
                  {row.sourceRevision}
                </dd>
              </div>
            ))}
          </dl>
        </details>
      </section>
      {sends}
    </>
  );
}
