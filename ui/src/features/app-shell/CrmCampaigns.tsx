import { useEffect, useRef, useState } from "react";
import { api } from "../../lib/api";
import type { Connection } from "../../lib/types";
import { smtpSummary } from "../settings/connectionsView";
import Button from "../../ui/Button";
import Select from "../../ui/Select";
import type { Viewer } from "../projects/work";
import {
  audienceClient,
  type AudienceBaseInput,
  type AudienceScope,
} from "./audienceClient";
import {
  checkCampaignId,
  checkContent,
  friendlyCampaignError,
  newRequestId,
  parseContentDoc,
  parseContentList,
  parseProposalList,
  parseProposalRequest,
  parseRender,
  type CampaignBlock,
  type ContentDoc,
  type ContentRender,
  type ProposalDoc,
  type ProposalRequestDoc,
} from "./campaignGrammar";
import { contentClient } from "./contentClient";
import {
  DELIVERY_CLAIM,
  friendlySendError,
  isNoSenderBound,
  parseOriginReceipt,
  parsePreparedSend,
  parseSendList,
  parseSendView,
  parseSmtpBinding,
  parseTestSendReceipt,
  sendClient,
  sendTerminal,
  type DeliveryRow,
  type PreparedSend,
  type SendListEntry,
  type SendView,
  type SmtpBinding,
  type TestSendReceipt,
} from "./sendClient";
import { PreviewPanel, parsePreview, type AudiencePreview } from "./CrmSegments";
import { friendlyAudienceError, newAudienceId } from "./segmentGrammar";
import Field from "./shared/Field";

/**
 * Campaign screens inside the trusted CRM shell (CAD-784 over the
 * CAD-780 audience engine and the CAD-782 versioned content store).
 * List, separate New page and direct detail pages — the list never
 * contains an inline builder. One base audience mode per campaign
 * (All, one saved segment, or a custom ID set) plus a saved
 * exclusion list, host preview counts, a constrained visual email
 * editor, host-rendered previews, a distinct test-send affordance,
 * and attributed proposals with explicit Apply/Discard.
 *
 * Honest boundaries, enforced by the contracts underneath:
 * - A campaign stores content (subject/preheader/blocks) plus a
 *   content-only approval. Audience freezes are context-scoped rows
 *   addressed by operator-chosen IDs — the host stores no
 *   campaign-to-audience link, so the detail names its freeze
 *   explicitly instead of pretending one is attached.
 * - Sending is two distinct operator actions (CAD-785/786): a real
 *   one-recipient SMTP test send whose receipt labels acceptance
 *   only, and a prepared-then-approved bounded send whose approve
 *   demands the operator type the final recipient count. SMTP
 *   acceptance is never presented as inbox delivery or a read.
 * - Assistant proposals ride the CAD-813 seam: the operator mints a
 *   one-time proposal request against their newest scope-stamped
 *   left-chat message, the assistant's turn redeems it once, and the
 *   proposal arrives with a durable host receipt. The "verified
 *   assistant draft" badge renders only from `actor: "assistant"`
 *   plus a non-null receipt — anything else is operator-submitted.
 *   The manual Submit stays for operator copy and is labelled as
 *   such; it never claims assistant provenance.
 *
 * The shell's own header row is the single Apps → App breadcrumb and
 * title; the section renders its own real heading instead of a
 * second crumb (CAD-863 release correction).
 */

export default function CrmCampaigns({
  scope,
  scopedChatMessage,
  viewer,
  view,
  recordId,
  onView,
  onSelect,
  onRecordCreated,
}: {
  scope: AudienceScope;
  /** CAD-813: the operator's newest left-chat message daemon-stamped
   *  with this exact install/context — the mint's `message_id`.
   *  Threaded down from the shell, never read from a global. */
  scopedChatMessage?: string | null;
  viewer: Viewer;
  view: "list" | "new";
  recordId: string | null;
  onView: (view: "list" | "new") => void;
  onSelect: (recordId: string | null) => void;
  onRecordCreated?: (recordId: string) => void;
}) {
  return (
    <div className="crm-list" data-section="campaigns">
      {recordId !== null ? (
        <CampaignDetail
          scope={scope}
          scopedChatMessage={scopedChatMessage ?? null}
          viewer={viewer}
          campaignId={recordId}
          onBack={() => onSelect(null)}
        />
      ) : view === "list" ? (
        <CampaignList
          scope={scope}
          viewer={viewer}
          onSelect={onSelect}
          onNew={() => onView("new")}
        />
      ) : (
        <CampaignNew
          scope={scope}
          scopedChatMessage={scopedChatMessage ?? null}
          viewer={viewer}
          onCreated={(id) => {
            if (onRecordCreated) onRecordCreated(id);
            else {
              onView("list");
              onSelect(id);
            }
          }}
          onCancel={() => onView("list")}
        />
      )}
    </div>
  );
}

function CampaignList({
  scope,
  viewer,
  onSelect,
  onNew,
}: {
  scope: AudienceScope;
  viewer: Viewer;
  onSelect: (campaignId: string) => void;
  onNew: () => void;
}) {
  const canWrite = viewer.operator && !viewer.readOnly;
  const [campaigns, setCampaigns] = useState<ContentDoc[]>([]);
  const [sends, setSends] = useState<SendListEntry[]>([]);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);
  const [retry, setRetry] = useState(0);

  const reloadToken = `${scope.installId}:${scope.contextId}:${retry}`;
  useEffect(() => {
    const controller = new AbortController();
    setLoading(true);
    setError(null);
    contentClient
      .list(scope)
      .then((value) => {
        if (!controller.signal.aborted) setCampaigns(parseContentList(value));
      })
      .catch((e: unknown) => {
        if (!controller.signal.aborted) setError(friendlyCampaignError(e));
      })
      .finally(() => {
        if (!controller.signal.aborted) setLoading(false);
      });
    return () => controller.abort();
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [reloadToken]);

  // Latest send per campaign — the same operator read the detail
  // page polls; its failure never blocks the content list.
  useEffect(() => {
    if (scope.contextId === "" || !viewer.operator) return;
    const controller = new AbortController();
    sendClient
      .sendList(scope)
      .then((value) => {
        if (!controller.signal.aborted) setSends(parseSendList(value));
      })
      .catch(() => {
        if (!controller.signal.aborted) setSends([]);
      });
    return () => controller.abort();
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [reloadToken]);

  // The newest send the host recorded per campaign — the list's
  // `created` order is the daemon's insertion order.
  const latestSendByCampaign = (() => {
    const map = new Map<string, SendListEntry>();
    for (const row of sends) map.set(row.campaignId, row);
    return map;
  })();

  return (
    <section aria-label="Campaigns list" className="crm-list">
      <h3 className="text-cardtitle font-medium text-ink-100" data-outlet-heading>
        Campaigns
      </h3>
      <div className="crm-toolbar mb-4">
        <p className="text-secondary text-ink-300">
          Versioned email content with content-only approval. Audience freezes and test-send
          receipts live on each campaign's page.
        </p>
        <span className="flex-1" />
        {canWrite && (
          <Button variant="primary" size="sm" onClick={onNew}>
            New campaign
          </Button>
        )}
      </div>
      {!viewer.operator && (
        <p className="card px-4 py-3 text-label text-ink-400">
          Sign in as the operator to inspect campaigns.
        </p>
      )}
      {viewer.operator && viewer.readOnly && (
        <p className="card px-4 py-3 text-label text-ink-400" data-state="read-only">
          Read-only view. Campaign creation and edits are unavailable.
        </p>
      )}
      {scope.contextId === "" && viewer.operator && (
        <p className="card px-4 py-3 text-label text-ink-400">
          Administrator CRM setup is required before campaigns open.
        </p>
      )}
      {scope.contextId !== "" && viewer.operator && loading && (
        <p className="text-secondary text-ink-400" role="status">
          Reading campaigns…
        </p>
      )}
      {scope.contextId !== "" && viewer.operator && error !== null && !loading && (
        <p className="card px-4 py-3 text-label text-fail border-fail/40" role="alert">
          {error}{" "}
          <button type="button" className="lnk" onClick={() => setRetry((count) => count + 1)}>
            Retry
          </button>
        </p>
      )}
      {scope.contextId !== "" && viewer.operator && error === null && !loading && campaigns.length === 0 && (
        <div className="card px-4 py-5 text-secondary text-ink-400" data-empty="campaigns" role="status">
          <p className="font-medium text-ink-200">No campaigns yet</p>
          <p className="mt-1">
            Create the first campaign with New campaign — audience, editor, preview and test-send
            all live there, never on this list. Only real server rows appear here.
          </p>
        </div>
      )}
      {scope.contextId !== "" && viewer.operator && error === null && campaigns.length > 0 && (
        <div
          className="crm-table-wrap"
          tabIndex={0}
          role="region"
          aria-label="Campaigns table — scroll horizontally to reach every column"
        >
          <table className="crm-table">
            <thead>
              <tr>
                <th scope="col">Subject</th>
                <th scope="col">Content</th>
                <th scope="col">Latest send</th>
                <th scope="col">
                  <span className="sr-only">Open</span>
                </th>
              </tr>
            </thead>
            <tbody>
              {campaigns.map((campaign) => (
                <tr key={campaign.campaignId}>
                  <td className="text-ink-100">
                    {campaign.subject}
                    <span className="num text-micro text-ink-500"> · {campaign.campaignId}</span>
                  </td>
                  <td>
                    <span
                      className="chip"
                      title={
                        campaign.approval.valid
                          ? `The saved content is approved${campaign.approval.revision !== campaign.revision ? " on an earlier saved version" : ""}`
                          : "The saved content is not approved yet"
                      }
                    >
                      {campaign.approval.valid
                        ? campaign.approval.revision === campaign.revision
                          ? "Approved"
                          : "Needs review — edited since approval"
                        : "Draft"}
                    </span>
                  </td>
                  <td className="num text-ink-300" data-send-state>
                    {(() => {
                      const send = latestSendByCampaign.get(campaign.campaignId);
                      if (send === undefined) {
                        return <span className="text-ink-500">—</span>;
                      }
                      if (send.state === "prepared") {
                        return (
                          <span
                            className="chip crm-send-chip"
                            data-state={send.state}
                            title="Prepared but not yet approved — nothing was sent"
                          >
                            Pending send
                          </span>
                        );
                      }
                      const total =
                        send.counts.queued +
                        send.counts.submitting +
                        send.counts.accepted +
                        send.counts.failed +
                        send.counts.uncertain +
                        send.counts.suppressed +
                        send.counts.closed;
                      return (
                        <span
                          className="chip crm-send-chip"
                          data-state={send.state}
                          title="SMTP acceptance only — not proof of inbox delivery"
                        >
                          {send.state} · {send.counts.accepted}/{total} accepted
                        </span>
                      );
                    })()}
                  </td>
                  <td>
                    <button type="button" className="lnk" onClick={() => onSelect(campaign.campaignId)}>
                      Open
                    </button>
                  </td>
                </tr>
              ))}
            </tbody>
          </table>
        </div>
      )}
    </section>
  );
}

/* ------------------------------------------------------------------ */
/* Audience: exactly one base mode plus a saved exclusion list.        */
/* ------------------------------------------------------------------ */

export interface AudiencePick {
  base: AudienceBaseInput;
  exclusionListId: string | null;
}

function parseIds(raw: string): string[] {
  return [...new Set(raw.split(/[\s,]+/).map((id) => id.trim()).filter((id) => id !== ""))];
}

function AudienceSection({
  scope,
  viewer,
  pick,
  onPick,
  onPreview,
  freezeSlot,
}: {
  scope: AudienceScope;
  viewer: Viewer;
  pick: AudiencePick;
  onPick: (pick: AudiencePick) => void;
  onPreview: (preview: AudiencePreview | null) => void;
  /** Optional freeze controls rendered under the preview. */
  freezeSlot?: React.ReactNode;
}) {
  const canWrite = viewer.operator && !viewer.readOnly;
  const [segments, setSegments] = useState<{ id: string; name: string }[]>([]);
  const [exclusions, setExclusions] = useState<{ id: string; name: string }[]>([]);
  const [customIds, setCustomIds] = useState("");
  const [listsError, setListsError] = useState<string | null>(null);
  const [preview, setPreview] = useState<AudiencePreview | null>(null);
  const [previewLoading, setPreviewLoading] = useState(false);
  const [previewError, setPreviewError] = useState<string | null>(null);
  const [previewToken, setPreviewToken] = useState(0);
  const [suppressions, setSuppressions] = useState<{ kind: string; key: string; reason: string }[]>([]);
  const [suppressionError, setSuppressionError] = useState<string | null>(null);
  const [suppressionToken, setSuppressionToken] = useState(0);
  const [showExclusionForm, setShowExclusionForm] = useState(false);
  const [exclusionName, setExclusionName] = useState("");
  const [exclusionIds, setExclusionIds] = useState("");
  const [exclusionPending, setExclusionPending] = useState(false);
  const [exclusionError, setExclusionError] = useState<string | null>(null);
  const [suppressEmail, setSuppressEmail] = useState("");
  const [suppressReason, setSuppressReason] = useState("");
  const [suppressPending, setSuppressPending] = useState(false);
  const [suppressError, setSuppressError] = useState<string | null>(null);

  const listsToken = `${scope.installId}:${scope.contextId}`;
  useEffect(() => {
    if (scope.contextId === "" || !viewer.operator) return;
    const controller = new AbortController();
    setListsError(null);
    Promise.all([audienceClient.segmentList(scope), audienceClient.exclusionList(scope)])
      .then(([segValue, excValue]) => {
        if (controller.signal.aborted) return;
        const segs = ((segValue as { segments?: unknown } | null)?.segments ?? []) as {
          id?: unknown;
          name?: unknown;
        }[];
        const excs = ((excValue as { exclusions?: unknown } | null)?.exclusions ?? []) as {
          id?: unknown;
          name?: unknown;
        }[];
        setSegments(
          Array.isArray(segs)
            ? segs.flatMap((row) =>
                typeof row.id === "string" && typeof row.name === "string"
                  ? [{ id: row.id, name: row.name }]
                  : [],
              )
            : [],
        );
        setExclusions(
          Array.isArray(excs)
            ? excs.flatMap((row) =>
                typeof row.id === "string" && typeof row.name === "string"
                  ? [{ id: row.id, name: row.name }]
                  : [],
              )
            : [],
        );
      })
      .catch((e: unknown) => {
        if (!controller.signal.aborted) setListsError(friendlyAudienceError(e));
      });
    return () => controller.abort();
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [listsToken, suppressionToken]);

  useEffect(() => {
    if (scope.contextId === "" || !viewer.operator) return;
    const controller = new AbortController();
    audienceClient
      .suppressionList(scope)
      .then((value) => {
        if (controller.signal.aborted) return;
        const rows = ((value as { suppressions?: unknown } | null)?.suppressions ?? []) as {
          kind?: unknown;
          key?: unknown;
          reason?: unknown;
        }[];
        setSuppressions(
          Array.isArray(rows)
            ? rows.flatMap((row) =>
                typeof row.kind === "string" &&
                typeof row.key === "string" &&
                typeof row.reason === "string"
                  ? [{ kind: row.kind, key: row.key, reason: row.reason }]
                  : [],
              )
            : [],
        );
        setSuppressionError(null);
      })
      .catch((e: unknown) => {
        if (!controller.signal.aborted) setSuppressionError(friendlyAudienceError(e));
      });
    return () => controller.abort();
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [listsToken, suppressionToken]);

  // Debounced host preview: exact counts and a bounded sample for the
  // one chosen base mode plus the saved exclusion list.
  const previewKey = JSON.stringify({ base: pick.base, exclusion: pick.exclusionListId });
  useEffect(() => {
    if (scope.contextId === "" || !viewer.operator) {
      setPreview(null);
      onPreview(null);
      return;
    }
    if (pick.base.mode === "custom" && pick.base.customerIds.length === 0) {
      setPreview(null);
      onPreview(null);
      setPreviewLoading(false);
      setPreviewError(null);
      return;
    }
    setPreviewLoading(true);
    const timer = setTimeout(() => {
      const controller = new AbortController();
      audienceClient
        .preview(scope, pick.base, pick.exclusionListId ?? undefined)
        .then((value) => {
          if (controller.signal.aborted) return;
          const parsed = parsePreview(value);
          setPreview(parsed);
          onPreview(parsed);
          setPreviewError(null);
        })
        .catch((e: unknown) => {
          if (controller.signal.aborted) return;
          setPreviewError(friendlyAudienceError(e));
          setPreview(null);
          onPreview(null);
        })
        .finally(() => {
          if (!controller.signal.aborted) setPreviewLoading(false);
        });
      return () => controller.abort();
    }, 400);
    return () => clearTimeout(timer);
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [previewKey, listsToken, previewToken]);

  const setMode = (mode: AudienceBaseInput["mode"]) => {
    if (mode === "all") onPick({ ...pick, base: { mode: "all" } });
    else if (mode === "segment") {
      const first = segments[0]?.id ?? "";
      onPick({ ...pick, base: first === "" ? { mode: "all" } : { mode: "segment", segmentId: first } });
    } else {
      onPick({ ...pick, base: { mode: "custom", customerIds: parseIds(customIds) } });
    }
  };

  return (
    <section aria-label="Audience" className="card px-4 py-4 grid gap-3">
      <h4 className="text-cardtitle font-medium text-ink-100">Audience — one base mode</h4>
      {listsError && (
        <p className="text-label text-fail" role="alert">
          {listsError}
        </p>
      )}
      <div role="radiogroup" aria-label="Base audience mode" className="crm-toolbar">
        {(
          [
            ["all", "All eligible customers"],
            ["segment", "One saved segment"],
            ["custom", "Custom customer IDs"],
          ] as [AudienceBaseInput["mode"], string][]
        ).map(([mode, label]) => (
          <label key={mode} className="text-label text-ink-200">
            <input
              type="radio"
              name="audience-base-mode"
              checked={pick.base.mode === mode}
              disabled={!canWrite || (mode === "segment" && segments.length === 0)}
              onChange={() => setMode(mode)}
            />{" "}
            {label}
            {mode === "segment" && segments.length === 0 ? " (none saved yet)" : ""}
          </label>
        ))}
      </div>
      {pick.base.mode === "segment" && (
        <div className="crm-field">
          <label className="text-label text-ink-300" htmlFor="aud-segment">
            Saved segment
          </label>
          <Select
            id="aud-segment"
            value={pick.base.segmentId}
            onChange={(value) => onPick({ ...pick, base: { mode: "segment", segmentId: value } })}
            options={segments.map((row) => ({ value: row.id, label: `${row.name} · ${row.id}` }))}
            aria-label="Saved segment"
            disabled={!canWrite}
            full
          />
        </div>
      )}
      {pick.base.mode === "custom" && (
        <div className="crm-field">
          <label className="text-label text-ink-300" htmlFor="aud-custom">
            Customer IDs (comma or space separated — the final suppression union still applies)
          </label>
          <textarea
            id="aud-custom"
            className="field"
            rows={2}
            value={customIds}
            disabled={!canWrite}
            onChange={(e) => {
              setCustomIds(e.target.value);
              onPick({ ...pick, base: { mode: "custom", customerIds: parseIds(e.target.value) } });
            }}
            maxLength={8000}
            autoComplete="off"
            placeholder="cust-abc123, cust-def456"
          />
        </div>
      )}
      <div className="crm-field">
        <label className="text-label text-ink-300" htmlFor="aud-exclusion">
          Saved exclusion list (applies on every base mode)
        </label>
        <Select
          id="aud-exclusion"
          value={pick.exclusionListId ?? ""}
          onChange={(value) => onPick({ ...pick, exclusionListId: value === "" ? null : value })}
          options={[
            { value: "", label: "No exclusion list" },
            ...exclusions.map((row) => ({ value: row.id, label: `${row.name} · ${row.id}` })),
          ]}
          aria-label="Saved exclusion list"
          disabled={!canWrite}
          full
        />
      </div>
      {canWrite && !showExclusionForm && (
        <p>
          <button type="button" className="lnk text-label" onClick={() => setShowExclusionForm(true)}>
            + New exclusion list
          </button>
        </p>
      )}
      {canWrite && showExclusionForm && (
        <form
          className="card px-3 py-3 grid gap-2"
          onSubmit={(e) => {
            e.preventDefault();
            setExclusionError(null);
            const ids = parseIds(exclusionIds);
            if (exclusionName.trim() === "" || ids.length === 0 || ids.length > 100) {
              setExclusionError("an exclusion list needs a name and 1 to 100 customer IDs");
              return;
            }
            setExclusionPending(true);
            const listId = newAudienceId("exc");
            void audienceClient
              .exclusionSave(scope, { listId, name: exclusionName.trim(), memberIds: ids })
              .then(() => {
                setShowExclusionForm(false);
                setExclusionName("");
                setExclusionIds("");
                setSuppressionToken((count) => count + 1);
                onPick({ ...pick, exclusionListId: listId });
              })
              .catch((err: unknown) => setExclusionError(friendlyAudienceError(err)))
              .finally(() => setExclusionPending(false));
          }}
        >
          <div className="crm-field-row">
            <div className="crm-field">
              <label className="text-label text-ink-300" htmlFor="aud-exc-name">
                List name
              </label>
              <input
                id="aud-exc-name"
                className="field"
                value={exclusionName}
                onChange={(e) => setExclusionName(e.target.value)}
                maxLength={80}
                autoComplete="off"
                disabled={exclusionPending}
              />
            </div>
            <div className="crm-field">
              <label className="text-label text-ink-300" htmlFor="aud-exc-ids">
                Member customer IDs
              </label>
              <input
                id="aud-exc-ids"
                className="field"
                value={exclusionIds}
                onChange={(e) => setExclusionIds(e.target.value)}
                maxLength={8000}
                autoComplete="off"
                disabled={exclusionPending}
                placeholder="cust-abc123, cust-def456"
              />
            </div>
          </div>
          {exclusionError && (
            <p className="text-label text-fail" role="alert">
              {exclusionError}
            </p>
          )}
          <div className="crm-toolbar">
            <Button type="submit" size="sm" loading={exclusionPending} disabled={exclusionPending}>
              Save exclusion list
            </Button>
            <button type="button" className="lnk text-label" onClick={() => setShowExclusionForm(false)}>
              Cancel
            </button>
          </div>
        </form>
      )}
      <PreviewPanel
        preview={preview}
        loading={previewLoading}
        error={previewError}
        onRetry={() => setPreviewToken((count) => count + 1)}
        label="Audience preview"
      />
      <section aria-label="Suppressions" className="mt-1">
        <h4 className="text-label font-medium text-ink-200">
          Suppressions ({suppressions.length}) — always excluded, even from custom IDs
        </h4>
        {suppressionError !== null ? (
          <p className="text-label text-fail" role="alert">
            {suppressionError}{" "}
            <button type="button" className="lnk" onClick={() => setSuppressionToken((count) => count + 1)}>
              Retry
            </button>
          </p>
        ) : suppressions.length === 0 ? (
          <p className="text-label text-ink-400">No suppressions in this context.</p>
        ) : (
          <ol className="crm-history">
            {suppressions.slice(0, 10).map((row) => (
              <li key={`${row.kind}:${row.key}`} className="num text-label text-ink-300">
                {row.kind} · {row.key} · {row.reason}
              </li>
            ))}
            {suppressions.length > 10 && (
              <li className="num text-label text-ink-500">…and {suppressions.length - 10} more</li>
            )}
          </ol>
        )}
        {canWrite && (
          <form
            className="crm-field-row mt-2"
            onSubmit={(e) => {
              e.preventDefault();
              setSuppressError(null);
              if (suppressEmail.trim() === "" || suppressReason.trim() === "") {
                setSuppressError("a suppression needs an email address and a reason");
                return;
              }
              setSuppressPending(true);
              void audienceClient
                .suppressionAdd(scope, { email: suppressEmail.trim(), reason: suppressReason.trim() })
                .then(() => {
                  setSuppressEmail("");
                  setSuppressReason("");
                  setSuppressionToken((count) => count + 1);
                  setPreviewToken((count) => count + 1);
                })
                .catch((err: unknown) => setSuppressError(friendlyAudienceError(err)))
                .finally(() => setSuppressPending(false));
            }}
          >
            <div className="crm-field">
              <label className="text-label text-ink-300" htmlFor="aud-suppress-email">
                Suppress an email
              </label>
              <input
                id="aud-suppress-email"
                className="field"
                type="email"
                value={suppressEmail}
                onChange={(e) => setSuppressEmail(e.target.value)}
                maxLength={254}
                autoComplete="off"
                disabled={suppressPending}
                placeholder="name@example.com"
              />
            </div>
            <div className="crm-field">
              <label className="text-label text-ink-300" htmlFor="aud-suppress-reason">
                Reason
              </label>
              <input
                id="aud-suppress-reason"
                className="field"
                value={suppressReason}
                onChange={(e) => setSuppressReason(e.target.value)}
                maxLength={80}
                autoComplete="off"
                disabled={suppressPending}
                placeholder="opted out by phone"
              />
            </div>
            {suppressError && (
              <p className="text-label text-fail" role="alert">
                {suppressError}
              </p>
            )}
            <div>
              <Button type="submit" size="sm" loading={suppressPending} disabled={suppressPending}>
                Add suppression
              </Button>
            </div>
          </form>
        )}
      </section>
      {freezeSlot}
    </section>
  );
}

/* ------------------------------------------------------------------ */
/* Visual email editor + host preview + test-send + proposals.         */
/* ------------------------------------------------------------------ */

interface EditorBlock {
  key: number;
  kind: CampaignBlock["type"];
  text: string;
  label: string;
  url: string;
}

let editorKey = 1;

function blocksFromDoc(doc: ContentDoc | null): EditorBlock[] {
  if (doc === null) return [{ key: editorKey++, kind: "paragraph", text: "", label: "", url: "" }];
  return doc.blocks.map((block) => {
    if (block.type === "button") {
      return { key: editorKey++, kind: "button" as const, text: "", label: block.label, url: block.url };
    }
    return { key: editorKey++, kind: block.type, text: block.text, label: "", url: "" };
  });
}

function blocksToGrammar(blocks: EditorBlock[]): CampaignBlock[] {
  return blocks.map((block) => {
    if (block.kind === "heading") return { type: "heading", text: block.text };
    if (block.kind === "paragraph") return { type: "paragraph", text: block.text };
    return { type: "button", label: block.label, url: block.url };
  });
}

/* ------------------------------------------------------------------ */
/* Send controls (CAD-785 binding + CAD-786 approved bounded send).     */
/* ------------------------------------------------------------------ */

/** `smtpShow` answers `null` on the host's none-bound refusal; every
 *  other refusal is an error the panel shows verbatim. */
function readBinding(scope: AudienceScope): Promise<SmtpBinding | null> {
  return sendClient.smtpShow(scope).then(parseSmtpBinding, (error: unknown) => {
    if (isNoSenderBound(error)) return null;
    throw error;
  });
}

/** A modal confirm: the operator's typed `expect` must equal
 *  `match` before `confirmLabel` enables. Esc cancels, the input
 *  autofocuses and a submit re-arms only through a fresh open. */
function ConfirmDialog({
  title,
  body,
  expect,
  match,
  inputLabel,
  confirmLabel,
  pending,
  error,
  onConfirm,
  onCancel,
}: {
  title: string;
  body: React.ReactNode;
  /** When set, the input gate: confirm enables only when the typed
   *  text equals `match` exactly. */
  expect?: string;
  match?: string;
  inputLabel?: string;
  confirmLabel: string;
  pending: boolean;
  error: string | null;
  onConfirm: () => void;
  onCancel: () => void;
}) {
  const [typed, setTyped] = useState("");
  const inputRef = useRef<HTMLInputElement | null>(null);
  useEffect(() => {
    if (expect !== undefined) inputRef.current?.focus();
  }, [expect]);
  const gated = expect !== undefined ? typed.trim() === (match ?? "") : true;
  return (
    <div className="crm-confirm-wrap" role="presentation">
      <div className="crm-confirm-scrim" onClick={onCancel} />
      <section
        className="card crm-confirm grid gap-3"
        role="dialog"
        aria-modal="true"
        aria-label={title}
        onKeyDown={(e) => {
          if (e.key === "Escape") {
            e.stopPropagation();
            onCancel();
          }
        }}
      >
        <h4 className="text-cardtitle font-medium text-ink-100">{title}</h4>
        <div className="text-label text-ink-300">{body}</div>
        {expect !== undefined && (
          <div className="crm-field">
            <label className="text-label text-ink-300" htmlFor="crm-confirm-input">
              {inputLabel ?? `Type ${match} to confirm`}
            </label>
            <input
              id="crm-confirm-input"
              ref={inputRef}
              className="field num"
              inputMode="numeric"
              value={typed}
              onChange={(e) => setTyped(e.target.value)}
              maxLength={12}
              autoComplete="off"
              disabled={pending}
            />
          </div>
        )}
        {error && (
          <p className="text-label text-fail" role="alert">
            {error}
          </p>
        )}
        <div className="crm-toolbar">
          <Button size="sm" onClick={onCancel} disabled={pending}>
            Cancel
          </Button>
          <Button
            size="sm"
            variant="danger"
            loading={pending}
            disabled={pending || !gated}
            onClick={onConfirm}
          >
            {confirmLabel}
          </Button>
        </div>
      </section>
    </div>
  );
}

/**
 * The bound SMTP sender: connection pick list (SMTP-enrolled rows
 * only), bind/rebind under link CAS, revoke behind a confirm, and
 * the effective unsubscribe origin with its operator override form.
 * Host refusals surface verbatim; nothing secret ever renders.
 */
function SenderBindPanel({
  scope,
  viewer,
  binding,
  onBinding,
}: {
  scope: AudienceScope;
  viewer: Viewer;
  binding: SmtpBinding | null;
  onBinding: (binding: SmtpBinding | null) => void;
}) {
  const canWrite = viewer.operator && !viewer.readOnly;
  const [connections, setConnections] = useState<Connection[]>([]);
  const [connError, setConnError] = useState<string | null>(null);
  const [picked, setPicked] = useState("");
  const [pending, setPending] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [note, setNote] = useState<string | null>(null);
  const [confirmRevoke, setConfirmRevoke] = useState(false);
  const [revokeError, setRevokeError] = useState<string | null>(null);

  const connToken = `${scope.installId}:${scope.contextId}`;
  useEffect(() => {
    if (scope.contextId === "" || !viewer.operator) return;
    const controller = new AbortController();
    api
      .connections()
      .then((value) => {
        if (controller.signal.aborted) return;
        setConnections(
          (value.connections ?? []).filter(
            (row) => (row.smtp ?? null) !== null && row.status.custody_available === true,
          ),
        );
        setConnError(null);
      })
      .catch((e: unknown) => {
        if (!controller.signal.aborted) setConnError(friendlySendError(e));
      });
    return () => controller.abort();
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [connToken]);

  useEffect(() => {
    if (picked === "" && connections.length > 0) setPicked(connections[0].id);
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [connections.length]);

  const run = (action: () => Promise<unknown>, ok: string) => {
    setPending(true);
    setError(null);
    setNote(null);
    void action()
      .then((value) => {
        onBinding(parseSmtpBinding(value));
        setNote(ok);
      })
      .catch((err: unknown) => setError(friendlySendError(err)))
      .finally(() => setPending(false));
  };

  return (
    <section aria-label="SMTP sender binding" className="grid gap-3">
      <h4 className="text-label font-medium text-ink-200">
        Sender — one host-custodied SMTP connection per context
      </h4>
      {connError && (
        <p className="text-label text-fail" role="alert">
          {connError}
        </p>
      )}
      {binding === null ? (
        <p className="text-label text-ink-400" data-state="unbound">
          No SMTP sender is bound to this installation and context. Bind one below — the binding
          pins the credential's authorization revision, so a rotation refuses sends until the
          operator rebinds.
        </p>
      ) : (
        <dl className="crm-detail" aria-label="Bound sender">
          <div>
            <dt>From</dt>
            <dd className="num">
              {binding.sender.name} · {binding.sender.address}
            </dd>
          </div>
          <div>
            <dt>Transport</dt>
            <dd className="num">
              {binding.transport.host}:{binding.transport.port} ·{" "}
              {binding.transport.tlsMode === "implicit" ? "implicit TLS" : "STARTTLS"} · login{" "}
              {binding.transport.username}
            </dd>
          </div>
          <div>
            <dt>Binding</dt>
            <dd className="num">
              link r{binding.linkRevision} · auth r{binding.authRevision} ·{" "}
              {binding.digest.slice(0, 18)}… · {binding.state}
            </dd>
          </div>
        </dl>
      )}
      {error && (
        <p className="text-label text-fail" role="alert">
          {error}
        </p>
      )}
      {note && (
        <p className="text-label text-ok" role="status">
          {note}
        </p>
      )}
      {canWrite && connections.length === 0 && connError === null && (
        <p className="text-label text-ink-500">
          No enrolled SMTP connections — enroll one under Settings → Connections first.
        </p>
      )}
      {canWrite && connections.length > 0 && (
        <div className="crm-field-row">
          <div className="crm-field">
            <label className="text-label text-ink-300" htmlFor="smtp-conn">
              SMTP connection
            </label>
            <Select
              id="smtp-conn"
              value={picked}
              onChange={setPicked}
              options={connections.map((row) => ({
                value: row.id,
                label: smtpSummary(row) ?? `${row.provider} · ${row.account}`,
              }))}
              aria-label="SMTP connection"
              disabled={pending}
              full
            />
          </div>
          <div>
            <span className="text-label text-ink-300">
              {binding === null ? "Bind" : `Rebind (expects link r${binding.linkRevision})`}
            </span>
            <div className="mt-1 crm-toolbar">
              {binding === null ? (
                <Button
                  size="sm"
                  variant="primary"
                  loading={pending}
                  disabled={pending || picked === ""}
                  onClick={() =>
                    run(
                      () => sendClient.smtpBind(scope, picked, newAudienceId("bind")),
                      "Sender bound — the link pins this credential's current authorization revision.",
                    )
                  }
                >
                  Bind sender
                </Button>
              ) : (
                <>
                  <Button
                    size="sm"
                    loading={pending}
                    disabled={pending || picked === ""}
                    title="Rebind under compare-and-set on the observed link revision"
                    onClick={() =>
                      run(
                        () => sendClient.smtpRebind(scope, picked, binding.linkRevision),
                        "Sender rebound — authorization revision re-pinned.",
                      )
                    }
                  >
                    Rebind sender
                  </Button>
                  <Button
                    size="sm"
                    variant="danger"
                    disabled={pending}
                    onClick={() => {
                      setRevokeError(null);
                      setConfirmRevoke(true);
                    }}
                  >
                    Revoke binding
                  </Button>
                </>
              )}
            </div>
          </div>
        </div>
      )}
      {confirmRevoke && binding !== null && (
        <ConfirmDialog
          title="Revoke the SMTP sender binding?"
          body={
            <p>
              This releases {binding.sender.address} from this context. Sends and test sends refuse
              until a sender is bound again; a prepared send loses its approval material.
            </p>
          }
          confirmLabel="Revoke binding"
          pending={pending}
          error={revokeError}
          onCancel={() => setConfirmRevoke(false)}
          onConfirm={() => {
            setPending(true);
            setRevokeError(null);
            void sendClient
              .smtpRevoke(scope, binding.linkRevision)
              .then(() => {
                onBinding(null);
                setConfirmRevoke(false);
                setNote("Sender binding revoked.");
              })
              .catch((err: unknown) => setRevokeError(friendlySendError(err)))
              .finally(() => setPending(false));
          }}
        />
      )}
      <OriginPanel viewer={viewer} />
    </section>
  );
}

/** The daemon-global unsubscribe origin: the base every minted
 *  `/unsubscribe/<token>` link is built on. The operator sets or
 *  clears it; validation failures surface verbatim from the host. */
function OriginPanel({ viewer }: { viewer: Viewer }) {
  const canWrite = viewer.operator && !viewer.readOnly;
  const [origin, setOrigin] = useState<string | null | undefined>(undefined);
  const [stored, setStored] = useState(false);
  const [input, setInput] = useState("");
  const [pending, setPending] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [note, setNote] = useState<string | null>(null);

  const read = () => {
    sendClient
      .sendOriginShow()
      .then((value) => {
        const receipt = parseOriginReceipt(value);
        setOrigin(receipt.unsubscribeOrigin);
        setStored(receipt.stored);
        setInput(receipt.unsubscribeOrigin ?? "");
      })
      .catch((e: unknown) => setError(friendlySendError(e)));
  };
  useEffect(() => {
    if (!viewer.operator) return;
    read();
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [viewer.operator]);

  const save = (value: string | null) => {
    setPending(true);
    setError(null);
    setNote(null);
    void sendClient
      .sendOriginSet(value)
      .then((receipt) => {
        const parsed = parseOriginReceipt(receipt);
        setOrigin(parsed.unsubscribeOrigin);
        setStored(parsed.stored);
        setInput(parsed.unsubscribeOrigin ?? "");
        setNote(
          parsed.unsubscribeOrigin === null
            ? "Unsubscribe origin cleared — sends refuse until one is configured."
            : `Unsubscribe origin set to ${parsed.unsubscribeOrigin}.`,
        );
      })
      .catch((err: unknown) => setError(friendlySendError(err)))
      .finally(() => setPending(false));
  };

  return (
    <section aria-label="Unsubscribe origin" className="grid gap-2">
      <h4 className="text-label font-medium text-ink-200">Unsubscribe origin</h4>
      {origin === undefined ? (
        <p className="text-label text-ink-500" role="status">
          Reading the effective origin…
        </p>
      ) : (
        <p className="text-label text-ink-300">
          Every send's unsubscribe links build on{" "}
          <span className="num">{origin ?? "nothing — sends refuse until one is set"}</span>
          {stored ? " (operator-set)" : origin !== null ? " (serve option)" : ""}.
        </p>
      )}
      {error && (
        <p className="text-label text-fail" role="alert">
          {error}
        </p>
      )}
      {note && (
        <p className="text-label text-ok" role="status">
          {note}
        </p>
      )}
      {canWrite && (
        <form
          className="crm-field-row"
          onSubmit={(e) => {
            e.preventDefault();
            save(input);
          }}
        >
          <div className="crm-field">
            <label className="text-label text-ink-300" htmlFor="unsub-origin">
              Origin (https; http on a loopback host)
            </label>
            <input
              id="unsub-origin"
              className="field"
              value={input}
              onChange={(e) => setInput(e.target.value)}
              maxLength={200}
              autoComplete="off"
              disabled={pending}
              placeholder="https://cadence.example.com"
            />
          </div>
          <div className="crm-toolbar" style={{ alignSelf: "end" }}>
            <Button type="submit" size="sm" loading={pending} disabled={pending}>
              Set origin
            </Button>
            {stored && (
              <Button size="sm" disabled={pending} onClick={() => save(null)}>
                Clear override
              </Button>
            )}
          </div>
        </form>
      )}
    </section>
  );
}

/** One recipient's masked row plus the uncertain-only resolve pair. */
function DeliveryRows({
  scope,
  viewer,
  view,
  onResolved,
}: {
  scope: AudienceScope;
  viewer: Viewer;
  view: SendView;
  onResolved: () => void;
}) {
  const canWrite = viewer.operator && !viewer.readOnly;
  const [confirm, setConfirm] = useState<{
    customerId: string;
    resolution: "accepted" | "failed";
  } | null>(null);
  const [pending, setPending] = useState(false);
  const [error, setError] = useState<string | null>(null);

  return (
    <>
      {error && (
        <p className="text-label text-fail" role="alert">
          {error}
        </p>
      )}
      <div
        className="crm-table-wrap"
        tabIndex={0}
        role="region"
        aria-label="Delivery rows — scroll horizontally to reach every column"
      >
        <table className="crm-table">
          <thead>
            <tr>
              <th scope="col">Recipient</th>
              <th scope="col">State</th>
              <th scope="col">Attempts</th>
              <th scope="col">SMTP</th>
              <th scope="col">Reason</th>
              <th scope="col">
                <span className="sr-only">Resolve</span>
              </th>
            </tr>
          </thead>
          <tbody>
            {view.deliveries.map((row: DeliveryRow) => (
              <tr key={row.customerId} data-delivery={row.customerId}>
                <td className="num text-ink-200">{row.email}</td>
                <td>
                  <span className="chip crm-send-chip" data-state={row.state}>
                    {row.state}
                  </span>
                  {row.resolvedBy !== null && (
                    <span className="text-micro text-ink-500"> · by {row.resolvedBy}</span>
                  )}
                </td>
                <td className="num text-ink-400">{row.attempts}</td>
                <td className="num text-ink-400">{row.smtpCode ?? "—"}</td>
                <td className="text-ink-400">{row.reason ?? "—"}</td>
                <td>
                  {canWrite && row.state === "uncertain" && (
                    <span className="crm-toolbar">
                      <button
                        type="button"
                        className="lnk"
                        onClick={() => {
                          setError(null);
                          setConfirm({ customerId: row.customerId, resolution: "accepted" });
                        }}
                      >
                        Mark accepted
                      </button>
                      <button
                        type="button"
                        className="lnk"
                        onClick={() => {
                          setError(null);
                          setConfirm({ customerId: row.customerId, resolution: "failed" });
                        }}
                      >
                        Mark failed
                      </button>
                    </span>
                  )}
                </td>
              </tr>
            ))}
          </tbody>
        </table>
      </div>
      {confirm !== null && (
        <ConfirmDialog
          title={`Mark this delivery ${confirm.resolution}?`}
          body={
            <p>
              The daemon lost the submission's answer, so this row is uncertain — the message may
              already have been sent. Marking it {confirm.resolution} records your reconciliation
              only: <strong>no resend happens either way</strong>.
            </p>
          }
          confirmLabel={`Mark ${confirm.resolution}`}
          pending={pending}
          error={null}
          onCancel={() => setConfirm(null)}
          onConfirm={() => {
            setPending(true);
            void sendClient
              .sendResolve(scope, view.send.sendId, confirm.customerId, confirm.resolution)
              .then(() => {
                setConfirm(null);
                onResolved();
              })
              .catch((err: unknown) => {
                setConfirm(null);
                setError(friendlySendError(err));
              })
              .finally(() => setPending(false));
          }}
        />
      )}
    </>
  );
}

/** Live progress after approval: poll `show` every 2s while the send
 *  is `sending` — stops on a terminal state, on unmount and at its
 *  own bound (5 minutes). Never a busy loop. */
function SendProgressPanel({
  scope,
  viewer,
  sendId,
}: {
  scope: AudienceScope;
  viewer: Viewer;
  sendId: string;
}) {
  const [view, setView] = useState<SendView | null>(null);
  const [error, setError] = useState<string | null>(null);
  const pollRef = useRef<{ timer: ReturnType<typeof setTimeout> | null }>({ timer: null });

  useEffect(() => {
    let stopped = false;
    let ticks = 0;
    const tick = () => {
      if (stopped) return;
      ticks += 1;
      sendClient
        .sendShow(scope, sendId)
        .then((value) => {
          if (stopped) return;
          const parsed = parseSendView(value);
          setView(parsed);
          setError(null);
          if (!sendTerminal(parsed.send.state) && ticks < 150) {
            pollRef.current.timer = setTimeout(tick, 2000);
          }
        })
        .catch((e: unknown) => {
          if (stopped) return;
          setError(friendlySendError(e));
          if (ticks < 150) pollRef.current.timer = setTimeout(tick, 2000);
        });
    };
    tick();
    return () => {
      stopped = true;
      if (pollRef.current.timer !== null) clearTimeout(pollRef.current.timer);
    };
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [sendId]);

  if (view === null && error === null) {
    return (
      <p className="text-label text-ink-400" role="status">
        Reading the send…
      </p>
    );
  }
  if (view === null) {
    return (
      <p className="text-label text-fail" role="alert">
        {error}
      </p>
    );
  }
  const counts = view.counts;
  const cells: [string, number][] = [
    ["prepared", view.send.state === "prepared" ? view.deliveries.length || counts.queued : view.deliveries.length],
    ["queued", counts.queued],
    ["suppressed", counts.suppressed],
    ["submitted", counts.submitting + counts.accepted + counts.failed + counts.uncertain],
    ["accepted", counts.accepted],
    ["failed", counts.failed],
    ["uncertain", counts.uncertain],
    ["closed", counts.closed],
  ];
  return (
    <section aria-label="Send progress" className="grid gap-3">
      <p className="text-label text-ink-300">
        <span className="chip crm-send-chip" data-state={view.send.state}>
          {view.send.state}
        </span>{" "}
        <span className="num">
          {view.send.sendId} · digest {view.send.sendDigest.slice(0, 18)}…
        </span>
        {view.send.closeReason !== null && (
          <span className="text-fail"> · {view.send.closeReason}</span>
        )}
      </p>
      <dl className="crm-detail" aria-label="Delivery counts">
        {cells.map(([label, count]) => (
          <div key={label}>
            <dt>{label}</dt>
            <dd className="num" data-count={label}>
              {count}
            </dd>
          </div>
        ))}
      </dl>
      <p className="text-micro text-ink-500">
        {DELIVERY_CLAIM === "smtp-acceptance-only" || view.deliveryClaim === DELIVERY_CLAIM
          ? "SMTP accepted the message — this is not proof of inbox delivery."
          : view.deliveryClaim}
      </p>
      {error && (
        <p className="text-label text-fail" role="alert">
          {error}
        </p>
      )}
      {view.deliveries.length > 0 && (
        <DeliveryRows
          scope={scope}
          viewer={viewer}
          view={view}
          onResolved={() => {
            sendClient
              .sendShow(scope, sendId)
              .then((value) => setView(parseSendView(value)))
              .catch(() => undefined);
          }}
        />
      )}
    </section>
  );
}

/**
 * The final-send section — visually distinct from the test send, it
 * gates Prepare on every prerequisite the host re-verifies (approved
 * content at the current revision, a valid named audience freeze, a
 * live sender binding, an accepted test send of this content+binding)
 * and shows which is missing before the wire ever sees a request.
 * Approve demands the operator type the final recipient count; the
 * body is exactly `{install_id, context_id, send_id, send_digest}`.
 * Any host refusal discards the prepared view — prepare again.
 */
function FinalSendPanel({
  scope,
  viewer,
  campaignId,
  doc,
  freezeId,
  freeze,
  binding,
  testEvidence,
}: {
  scope: AudienceScope;
  viewer: Viewer;
  campaignId: string;
  doc: ContentDoc;
  freezeId: string;
  freeze: { valid: boolean | null } | null;
  binding: SmtpBinding | null | undefined;
  testEvidence: TestSendReceipt | null;
}) {
  const canWrite = viewer.operator && !viewer.readOnly;
  const [prepared, setPrepared] = useState<PreparedSend | null>(null);
  const [preparePending, setPreparePending] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [confirmApprove, setConfirmApprove] = useState(false);
  const [approvePending, setApprovePending] = useState(false);
  const [approveError, setApproveError] = useState<string | null>(null);
  const [approvedSendId, setApprovedSendId] = useState<string | null>(null);

  const missing: string[] = [];
  if (!doc.approval.valid || doc.approval.revision !== doc.revision) {
    missing.push("content approved at the current revision");
  }
  if (freezeId.trim() === "") {
    missing.push("a named audience freeze");
  } else if (freeze === null) {
    missing.push(`freeze ${freezeId.trim()} rechecked below (its validity is unverified)`);
  } else if (freeze.valid !== true) {
    missing.push(`freeze ${freezeId.trim()} reporting valid`);
  }
  if (binding === undefined) {
    missing.push("the sender binding read (still loading)");
  } else if (binding === null || binding.state !== "live") {
    missing.push("a live SMTP sender binding");
  }
  if (testEvidence === null) {
    missing.push("an accepted test send of this content and binding");
  } else if (
    binding !== null &&
    binding !== undefined &&
    (testEvidence.contentDigest !== doc.contentDigest || testEvidence.linkDigest !== binding.digest)
  ) {
    missing.push("a test send accepted against this exact content revision and binding");
  }
  const canPrepare = canWrite && missing.length === 0;

  const prepare = () => {
    setPreparePending(true);
    setError(null);
    setPrepared(null);
    setApprovedSendId(null);
    void sendClient
      .sendPrepare(scope, campaignId, freezeId.trim(), newAudienceId("send"))
      .then((value) => setPrepared(parsePreparedSend(value)))
      .catch((err: unknown) => setError(friendlySendError(err)))
      .finally(() => setPreparePending(false));
  };

  return (
    <section aria-label="Final send" className="card px-4 py-4 grid gap-3 crm-send">
      <h4 className="text-cardtitle font-medium text-ink-100">
        Final send — approved, bounded, no resend
      </h4>
      <p className="text-label text-ink-400">
        Prepare commits content revision + digest, the audience freeze, the sender link and the
        unsubscribe origin into one send digest the operator then approves. Any material change
        between prepare and approve refuses the send. SMTP acceptance is recorded — never inbox
        delivery, never reads.
      </p>
      {missing.length > 0 ? (
        <div className="card px-3 py-3" data-prerequisites="missing">
          <p className="text-label text-ink-300">Prepare stays unavailable — missing:</p>
          <ul className="crm-history">
            {missing.map((item) => (
              <li key={item} className="text-label text-warn">
                · {item}
              </li>
            ))}
          </ul>
        </div>
      ) : (
        <p className="text-label text-ok" data-prerequisites="met">
          Every prerequisite is in place — approved r{doc.revision}, freeze {freezeId.trim()},
          sender {binding?.sender.address}, accepted test send of this content.
        </p>
      )}
      {error && (
        <p className="text-label text-fail" role="alert">
          {error}{" "}
          {prepared === null && <span className="text-ink-500">Prepare again.</span>}
        </p>
      )}
      {canWrite && (
        <div>
          <Button
            size="sm"
            variant="primary"
            loading={preparePending}
            disabled={!canPrepare || preparePending}
            title={
              canPrepare
                ? "Freeze the send's material and show what approval commits"
                : `Missing: ${missing.join("; ")}`
            }
            onClick={prepare}
          >
            Prepare send
          </Button>
        </div>
      )}
      {prepared !== null && (
        <section aria-label="Prepared send" className="card px-3 py-3 grid gap-3" data-prepared>
          <p className="text-label text-ink-200">
            Send <span className="num">{prepared.send.sendId}</span> — state{" "}
            <span className="chip crm-send-chip" data-state={prepared.send.state}>
              {prepared.send.state}
            </span>
          </p>
          <dl className="crm-detail" aria-label="Prepared counts">
            <div>
              <dt>Included</dt>
              <dd className="num" data-count="included">
                {prepared.counts.included}
              </dd>
            </div>
            <div>
              <dt>Excluded</dt>
              <dd className="num" data-count="excluded">
                {prepared.counts.excluded}
              </dd>
            </div>
            <div>
              <dt>Suppressed now</dt>
              <dd className="num" data-count="suppressed_now">
                {prepared.counts.suppressedNow}
              </dd>
            </div>
            <div>
              <dt>Final recipients</dt>
              <dd className="num" data-count="final">
                {prepared.counts.final} / ceiling {prepared.counts.maxRecipients}
              </dd>
            </div>
            <div>
              <dt>Content</dt>
              <dd className="num">
                r{prepared.send.contentRevision} · {prepared.send.contentDigest.slice(0, 18)}…
              </dd>
            </div>
            <div>
              <dt>Audience freeze</dt>
              <dd className="num">
                {prepared.send.audienceFreezeId} · {prepared.send.audienceDigest.slice(0, 18)}…
              </dd>
            </div>
            <div>
              <dt>Sender</dt>
              <dd className="num">
                {prepared.send.connectionId} · link r{prepared.send.linkRevision}
              </dd>
            </div>
            <div>
              <dt>Unsubscribe origin</dt>
              <dd className="num">{prepared.send.unsubscribeOrigin}</dd>
            </div>
            <div>
              <dt>Send digest</dt>
              <dd className="num" title={prepared.sendDigest}>
                {prepared.sendDigest.slice(0, 24)}…
              </dd>
            </div>
          </dl>
          {prepared.sample.length > 0 && (
            <p className="text-micro text-ink-500">
              Sample (masked):{" "}
              {prepared.sample.map((row) => `${row.customerId} · ${row.email}`).join(", ")}
            </p>
          )}
          <div className="crm-toolbar">
            <Button
              size="sm"
              variant="danger"
              disabled={approvePending}
              onClick={() => {
                setApproveError(null);
                setConfirmApprove(true);
              }}
            >
              Approve and send…
            </Button>
            <button
              type="button"
              className="lnk text-label"
              onClick={() => setPrepared(null)}
            >
              Discard prepared view
            </button>
          </div>
        </section>
      )}
      {confirmApprove && prepared !== null && (
        <ConfirmDialog
          title={`Approve send of ${prepared.counts.final} recipients?`}
          body={
            <>
              <p>
                This approves send <span className="num">{prepared.send.sendId}</span> exactly as
                prepared — {prepared.counts.final} recipients, content r
                {prepared.send.contentRevision}, digest{" "}
                <span className="num">{prepared.sendDigest.slice(0, 24)}…</span>. The host
                re-verifies every input; a refusal discards this prepared view.
              </p>
              <p className="text-micro text-ink-500">
                SMTP acceptance is recorded per recipient — it is not proof of inbox delivery.
              </p>
            </>
          }
          expect="count"
          match={String(prepared.counts.final)}
          inputLabel={`Type ${prepared.counts.final} (the final recipient count) to approve`}
          confirmLabel="Approve and send"
          pending={approvePending}
          error={approveError}
          onCancel={() => setConfirmApprove(false)}
          onConfirm={() => {
            setApprovePending(true);
            setApproveError(null);
            void sendClient
              .sendApprove(scope, prepared.send.sendId, prepared.sendDigest)
              .then((value) => {
                const view = parseSendView(value);
                setApprovedSendId(view.send.sendId);
                setPrepared(null);
                setConfirmApprove(false);
              })
              .catch((err: unknown) => {
                // Any host refusal (stale content/audience/sender/
                // origin) discards the prepared view — prepare again.
                setApproveError(friendlySendError(err));
                setPrepared(null);
                setConfirmApprove(false);
                setError(friendlySendError(err));
              })
              .finally(() => setApprovePending(false));
          }}
        />
      )}
      {approvedSendId !== null && (
        <SendProgressPanel scope={scope} viewer={viewer} sendId={approvedSendId} />
      )}
    </section>
  );
}

function CampaignWorkspace({
  scope,
  scopedChatMessage,
  viewer,
  campaignId,
  doc,
  onDoc,
  freezeId,
  freeze,
  audienceSlot,
}: {
  scope: AudienceScope;
  scopedChatMessage: string | null;
  viewer: Viewer;
  campaignId: string;
  doc: ContentDoc | null;
  onDoc: (doc: ContentDoc) => void;
  /** The detail page's named freeze + its last validity recheck —
   *  the final-send gate reads both. */
  freezeId?: string;
  freeze?: { valid: boolean | null } | null;
  /** CAD-1008: the saved campaign's audience/freeze panel, supplied by
   *  the owning page so the task order renders Content+Preview →
   *  approval → Audience → Sender → Test → Proposals → Final send.
   *  `undefined` on the new-campaign page, which keeps its own layout. */
  audienceSlot?: React.ReactNode;
}) {
  const canWrite = viewer.operator && !viewer.readOnly;
  const [subject, setSubject] = useState(doc?.subject ?? "");
  const [preheader, setPreheader] = useState(doc?.preheader ?? "");
  const [blocks, setBlocks] = useState<EditorBlock[]>(() => blocksFromDoc(doc));
  const [pending, setPending] = useState(false);
  const [formError, setFormError] = useState<string | null>(null);
  const [savedNote, setSavedNote] = useState<string | null>(null);
  const [render, setRender] = useState<ContentRender | null>(null);
  const [renderPending, setRenderPending] = useState(false);
  const [renderError, setRenderError] = useState<string | null>(null);
  const [renderToken, setRenderToken] = useState(0);
  const [previewTab, setPreviewTab] = useState<"visual" | "html" | "text">("visual");
  const [sampleName, setSampleName] = useState("");
  const [approvePending, setApprovePending] = useState(false);
  const [approveError, setApproveError] = useState<string | null>(null);
  const [testEmail, setTestEmail] = useState("");
  const [testPending, setTestPending] = useState(false);
  const [testError, setTestError] = useState<string | null>(null);
  const [testReceipt, setTestReceipt] = useState<TestSendReceipt | null>(null);
  // The live sender binding: `null` is the host's none-bound answer,
  // `undefined` is still-loading so dependent panels can wait.
  const [binding, setBinding] = useState<SmtpBinding | null | undefined>(undefined);
  const [bindingError, setBindingError] = useState<string | null>(null);
  const [bindingToken, setBindingToken] = useState(0);
  const [proposals, setProposals] = useState<ProposalDoc[]>([]);
  const [proposalsError, setProposalsError] = useState<string | null>(null);
  const [proposalToken, setProposalToken] = useState(0);
  const [proposalError, setProposalError] = useState<string | null>(null);
  const [proposalNote, setProposalNote] = useState<string | null>(null);
  // CAD-813: the minted request's host stamp, plus the bounded poll
  // that watches for the assistant's turn to redeem it.
  const [minted, setMinted] = useState<ProposalRequestDoc | null>(null);
  const [mintPending, setMintPending] = useState(false);
  const [mintWatching, setMintWatching] = useState(false);
  const mintPoll = useRef<{ deadline: number; requestId: string } | null>(null);

  // The doc is the saved truth: editor follows a newly saved revision
  // (create, Apply) but never clobbers typing mid-draft.
  const docIdentity = doc === null ? "none" : `${doc.revision}:${doc.contentDigest}`;
  useEffect(() => {
    if (doc === null) return;
    setSubject(doc.subject);
    setPreheader(doc.preheader);
    setBlocks(blocksFromDoc(doc));
    setTestReceipt(null);
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [docIdentity]);

  // Saved-version email preview (CAD-1008): the host-rendered saved
  // revision loads automatically on open, on every successful save and
  // on Refresh — never the unsaved editor bytes. The async answer is
  // dropped whenever the mounted campaign/revision or the request
  // generation moved on, so a stale render can never claim the new
  // saved revision.
  const renderLive = useRef<{ campaign: string; doc: string; seq: number }>({
    campaign: campaignId,
    doc: docIdentity,
    seq: 0,
  });
  renderLive.current.campaign = campaignId;
  renderLive.current.doc = docIdentity;
  useEffect(() => {
    if (doc === null) {
      setRender(null);
      setRenderPending(false);
      setRenderError(null);
      return;
    }
    const seq = ++renderLive.current.seq;
    const key = { campaign: campaignId, doc: docIdentity };
    // The revision+digest the render is asked for — a server-side
    // concurrent save can answer with newer content even when the
    // client generation still matches, so the receipt is validated
    // against the doc it was requested for before it can claim it.
    const expected = doc === null ? null : { revision: doc.revision, digest: doc.contentDigest };
    setRender(null);
    setRenderPending(true);
    setRenderError(null);
    const controller = new AbortController();
    contentClient
      .render(scope, campaignId, {
        sampleFirstName: sampleName.trim() === "" ? undefined : sampleName.trim(),
      })
      .then((value) => {
        const live = renderLive.current;
        if (controller.signal.aborted || live.seq !== seq || live.campaign !== key.campaign || live.doc !== key.doc) return;
        const next = parseRender(value);
        if (expected !== null && (next.revision !== expected.revision || next.contentDigest !== expected.digest)) {
          // A concurrent save moved the revision past the request:
          // surface the mismatch as a reload-needed state, never as
          // the current editor's render.
          setRender(null);
          setRenderError(
            `The saved campaign changed to r${next.revision} while the preview rendered — reload the campaign to see the current email.`,
          );
          return;
        }
        setRender(next);
      })
      .catch((err: unknown) => {
        const live = renderLive.current;
        if (controller.signal.aborted || live.seq !== seq || live.campaign !== key.campaign || live.doc !== key.doc) return;
        setRenderError(friendlyCampaignError(err));
      })
      .finally(() => {
        const live = renderLive.current;
        if (!controller.signal.aborted && live.seq === seq && live.campaign === key.campaign && live.doc === key.doc) {
          setRenderPending(false);
        }
      });
    return () => controller.abort();
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [scope.installId, scope.contextId, campaignId, docIdentity, renderToken]);

  // The sender binding is host state — read on mount and whenever a
  // bind/rebind/revoke lands (`bindingToken`). A none-bound refusal
  // reads as `null`, everything else surfaces verbatim.
  useEffect(() => {
    if (scope.contextId === "" || !viewer.operator) return;
    const controller = new AbortController();
    readBinding(scope)
      .then((value) => {
        if (!controller.signal.aborted) {
          setBinding(value);
          setBindingError(null);
        }
      })
      .catch((e: unknown) => {
        if (!controller.signal.aborted) setBindingError(friendlySendError(e));
      });
    return () => controller.abort();
    // eslint-disable-next-line react-hooks/exhaustive-deps
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [scope.installId, scope.contextId, viewer.operator, bindingToken]);

  const proposalsKey = `${scope.installId}:${scope.contextId}:${campaignId}:${proposalToken}`;
  useEffect(() => {
    // CAD-1013: proposals must be listed even before revision 1 exists —
    // the assistant-apply path is the creation route now, so a verified
    // proposal at source_revision 0 has to surface here for Apply.
    if (!viewer.operator) {
      setProposals([]);
      return;
    }
    const controller = new AbortController();
    contentClient
      .proposalList(scope)
      .then((value) => {
        if (!controller.signal.aborted) {
          setProposals(parseProposalList(value).filter((row) => row.campaignId === campaignId));
          setProposalsError(null);
        }
      })
      .catch((e: unknown) => {
        if (!controller.signal.aborted) setProposalsError(friendlyCampaignError(e));
      });
    return () => controller.abort();
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [proposalsKey]);

  // Bounded post-mint watch: after a proposal request lands, poll the
  // list every 3s until the assistant's receipt names it, the request
  // is spent, or two minutes pass — then stop. No busy loop, and the
  // watch dies with the workspace.
  useEffect(() => {
    if (!mintWatching || minted === null) return;
    const timer = setInterval(() => {
      const watch = mintPoll.current;
      if (watch === null || watch.requestId !== minted.requestId || Date.now() >= watch.deadline) {
        setMintWatching(false);
        return;
      }
      contentClient
        .proposalList(scope)
        .then((value) => {
          const matched = parseProposalList(value).some(
            (row) =>
              row.campaignId === campaignId &&
              row.assistantReceipt?.requestId === watch.requestId,
          );
          if (matched) {
            mintPoll.current = null;
            setMintWatching(false);
            setProposalToken((count) => count + 1);
            setProposalNote(
              `Verified assistant draft landed for request ${watch.requestId} — review and Apply or Discard below.`,
            );
            return;
          }
          setProposalToken((count) => count + 1);
        })
        .catch(() => {
          /* a transient read failure retries on the next tick */
        });
    }, 3000);
    return () => clearInterval(timer);
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [mintWatching, minted?.requestId]);

  // CAD-1013: the assistant can mint a request even before revision 1
  // exists — the host stamps source_revision=0 and Apply creates the
  // first revision, so the preview-first flow never strands creation.
  const mintRequest = () => {
    if (scopedChatMessage === null) return;
    setMintPending(true);
    setProposalError(null);
    setProposalNote(null);
    const requestId = newRequestId();
    void contentClient
      .proposalRequest(scope, {
        campaignId,
        messageId: scopedChatMessage,
        requestId,
      })
      .then((value) => {
        const request = parseProposalRequest(value);
        setMinted(request);
        mintPoll.current = { deadline: Date.now() + 120_000, requestId: request.requestId };
        setMintWatching(true);
        setProposalNote(
          `Request ${request.requestId} minted against chat message ${request.messageId} — the assistant's next turn can attach one draft to it.`,
        );
      })
      .catch((err: unknown) => setProposalError(friendlyCampaignError(err)))
      .finally(() => setMintPending(false));
  };

  const grammarBlocks = (): CampaignBlock[] => blocksToGrammar(blocks);

  const save = (e: React.FormEvent) => {
    e.preventDefault();
    setFormError(null);
    setSavedNote(null);
    try {
      checkCampaignId(campaignId);
      checkContent(subject, preheader, grammarBlocks());
    } catch (err: unknown) {
      setFormError(friendlyCampaignError(err));
      return;
    }
    setPending(true);
    void contentClient
      .save(scope, {
        campaignId,
        subject,
        preheader,
        blocks: grammarBlocks(),
        ...(doc === null ? {} : { expectedRevision: doc.revision }),
      })
      .then((value) => {
        const next = parseContentDoc(value);
        onDoc(next);
        setRender(null);
        setTestReceipt(null);
        setEditing(false);
        setSavedNote(
          doc === null
            ? `Created revision ${next.revision} — earlier content approval does not exist yet.`
            : `Saved revision ${next.revision} — content approval invalidated.`,
        );
      })
      .catch((err: unknown) => {
        // expectedRevision conflict: keep the operator's local text edits
        // open and ask for a reload — never overwrite an agent's newer draft.
        setFormError(
          friendlyCampaignError(err) +
            " Reload the draft to retry against the newer revision.",
        );
      })
      .finally(() => setPending(false));
  };

  // The editor diverged from the saved revision: the preview keeps
  // naming the last saved bytes instead of implying live content.
  // Blocks compare semantically — field order over the wire is not
  // the draft's identity.
  const blockKey = (block: CampaignBlock) =>
    block.type === "button" ? `button:${block.label}${block.url}` : `${block.type}:${block.text}`;
  const savedBlocks = doc?.blocks.map(blockKey) ?? [];
  const draftBlocks = blocksToGrammar(blocks).map(blockKey);
  const dirty =
    doc !== null &&
    (subject !== doc.subject ||
      preheader !== doc.preheader ||
      savedBlocks.length !== draftBlocks.length ||
      savedBlocks.some((key, index) => key !== draftBlocks[index]));

  const updateBlock = (key: number, patch: Partial<EditorBlock>) => {
    setBlocks((prev) => prev.map((block) => (block.key === key ? { ...block, ...patch } : block)));
  };

  const previewBody =
    doc === null ? (
      <p className="text-label text-ink-400" data-preview="unsaved">
        No saved email yet. Ask the assistant in the left chat to draft this campaign&apos;s
        email, then Apply its verified proposal below to create revision 1.
      </p>
    ) : (
      <>
        <div className="crm-field-row">
          <div className="crm-field">
            <label className="text-label text-ink-300" htmlFor="cmp-sample">
              Sample first name (optional)
            </label>
            <input
              id="cmp-sample"
              className="field"
              value={sampleName}
              onChange={(e) => setSampleName(e.target.value)}
              maxLength={40}
              autoComplete="off"
              placeholder="Ada"
            />
          </div>
          <div>
            <span className="text-label text-ink-300">Saved render</span>
            <div className="mt-1">
              <Button
                size="sm"
                loading={renderPending}
                disabled={renderPending}
                title="Re-render the last saved version with the current sample name"
                onClick={() => setRenderToken((count) => count + 1)}
              >
                Refresh preview
              </Button>
            </div>
          </div>
        </div>
        {renderError !== null && !renderPending && (
          <p className="text-label text-fail" role="alert">
            {renderError}{" "}
            <button type="button" className="lnk" onClick={() => setRenderToken((count) => count + 1)}>
              Retry
            </button>
          </p>
        )}
        {render === null && renderPending && (
          <p className="text-label text-ink-500" role="status" data-preview="loading">
            Rendering the saved email…
          </p>
        )}
        {dirty && (
          <p className="text-micro text-ink-500" data-preview="dirty">
            Preview shows the last saved version. Save changes to refresh.
          </p>
        )}
        {render !== null && (
          <>
            <p className="text-label text-ink-300">
              <span className="chip" title="Sender material is host-locked preview-only bytes">
                preview-only
              </span>{" "}
              <span className="num">
                {render.sender.name} · {render.sender.address}
              </span>
            </p>
            <div className="app-outlet-tabs" role="tablist" aria-label="Preview format">
              {(
                [
                  ["visual", "Visual"],
                  ["html", "HTML"],
                  ["text", "Text"],
                ] as ["visual" | "html" | "text", string][]
              ).map(([tab, label]) => (
                <button
                  key={tab}
                  type="button"
                  role="tab"
                  aria-selected={previewTab === tab}
                  className="app-outlet-tab"
                  data-on={previewTab === tab || undefined}
                  onClick={() => setPreviewTab(tab)}
                >
                  {label}
                </button>
              ))}
            </div>
            {previewTab === "visual" && (
              <iframe
                title={`Visual email preview, saved revision ${render.revision}`}
                sandbox=""
                srcDoc={render.html}
                className="crm-preview-frame"
                data-preview="visual"
              />
            )}
            {previewTab !== "visual" && (
              <pre className="crm-preview" data-preview={previewTab}>
                {previewTab === "html" ? render.html : render.text}
              </pre>
            )}
            <SenderPanel render={render} />
          </>
        )}
      </>
    );

  const previewSection = (
    <section aria-label="Email preview" className="card px-4 py-4 grid gap-3">
      <h4 className="text-cardtitle font-medium text-ink-100">
        Preview{doc !== null ? " — saved version" : ""}
      </h4>
      {previewBody}
    </section>
  );

  // CAD-1013 preview-first: the manual block composer is replaced by a
  // read-oriented content card. The email is created/refined by the
  // assistant proposal flow (Proposals section); the only operator edit
  // here is a bounded inline text correction of saved subject, preheader
  // and the text of existing blocks — never block structure, URLs or
  // token tooling. Corrections go through the same revisioned save path
  // (expectedRevision) so a conflict never overwrites an agent draft.
  const [editing, setEditing] = useState(false);
  const startEdit = () => {
    setSubject(doc?.subject ?? "");
    setPreheader(doc?.preheader ?? "");
    setBlocks(blocksFromDoc(doc));
    setFormError(null);
    setEditing(true);
  };
  const cancelEdit = () => {
    setBlocks(blocksFromDoc(doc));
    setSubject(doc?.subject ?? "");
    setPreheader(doc?.preheader ?? "");
    setFormError(null);
    setEditing(false);
  };
  const contentForm = (
      <div className="card px-4 py-4 grid gap-3" aria-label="Email content">
        <h4 className="text-cardtitle font-medium text-ink-100">
          Content {doc === null ? "— not drafted yet" : `— revision ${doc.revision}`}
        </h4>
        {doc === null && (
          <p className="text-label text-ink-400" data-state="no-draft">
            No email draft yet. Ask the assistant in the left chat to draft this campaign&apos;s
            email, then Apply its verified proposal below to create revision 1 — or use the
            inline editor after a draft exists.
          </p>
        )}
        {!editing ? (
          <>
            {doc !== null && (
              <dl className="sdetail" data-content-summary>
                <div>
                  <dt>Subject</dt>
                  <dd>{doc.subject}</dd>
                </div>
                <div>
                  <dt>Preheader</dt>
                  <dd>{doc.preheader === "" ? "—" : doc.preheader}</dd>
                </div>
                <div>
                  <dt>Blocks</dt>
                  <dd>
                    {doc.blocks.length} block{doc.blocks.length === 1 ? "" : "s"} · actor{" "}
                    {doc.actor} · {doc.contentDigest.slice(0, 18)}…
                  </dd>
                </div>
              </dl>
            )}
            {canWrite && doc !== null && (
              <div className="crm-toolbar">
                <Button type="button" size="sm" onClick={startEdit}>
                  Edit subject / text
                </Button>
                <span className="text-micro text-ink-500">
                  Text corrections save a new revision and invalidate the current approval.
                </span>
              </div>
            )}
            {doc !== null && !canWrite && (
              <p className="text-label text-ink-400" data-state="read-only">
                Read-only view. A verified operator saves content revisions.
              </p>
            )}
          </>
        ) : (
          <form className="grid gap-3" aria-label="Correct email text" onSubmit={save}>
            <div className="crm-field-row">
              <Field
                label="Subject"
                id="cmp-subject"
                hint="Plain text"
                required
                disabled={!canWrite || pending}
                className="crm-field"
              >
                {(c) => (
                  <input
                    {...c}
                    className="field"
                    value={subject}
                    onChange={(e) => setSubject(e.target.value)}
                    maxLength={150}
                    autoComplete="off"
                  />
                )}
              </Field>
              <Field
                label="Preheader"
                id="cmp-preheader"
                hint="Optional, plain text"
                disabled={!canWrite || pending}
                className="crm-field"
              >
                {(c) => (
                  <input
                    {...c}
                    className="field"
                    value={preheader}
                    onChange={(e) => setPreheader(e.target.value)}
                    maxLength={200}
                    autoComplete="off"
                  />
                )}
              </Field>
            </div>
            <ol className="crm-history" aria-label="Block text">
              {blocks.map((block, index) => (
                <li key={block.key} className="card px-3 py-3">
                  <p className="text-micro text-ink-500">
                    Block {index + 1} — {block.kind}
                    {block.kind === "button" ? " (URL unchanged)" : ""}
                  </p>
                  {block.kind === "button" ? (
                    <Field
                      label="Button label"
                      id={`cmp-block-label-${block.key}`}
                      disabled={!canWrite || pending}
                      className="crm-field mt-2"
                    >
                      {(c) => (
                        <input
                          {...c}
                          className="field"
                          value={block.label}
                          onChange={(e) => updateBlock(block.key, { label: e.target.value })}
                          maxLength={60}
                          autoComplete="off"
                        />
                      )}
                    </Field>
                  ) : (
                    <Field
                      label={block.kind === "heading" ? "Heading text" : "Paragraph text"}
                      id={`cmp-block-text-${block.key}`}
                      disabled={!canWrite || pending}
                      className="crm-field mt-2"
                    >
                      {(c) => (
                        <textarea
                          {...c}
                          className="field"
                          rows={block.kind === "heading" ? 2 : 4}
                          value={block.text}
                          onChange={(e) => updateBlock(block.key, { text: e.target.value })}
                          maxLength={block.kind === "heading" ? 120 : 2000}
                          autoComplete="off"
                        />
                      )}
                    </Field>
                  )}
                </li>
              ))}
            </ol>
            {formError && (
              <p className="text-label text-fail" role="alert">
                {formError}
              </p>
            )}
            {savedNote && (
              <p className="text-label text-ok" role="status">
                {savedNote}
              </p>
            )}
            <div className="crm-toolbar">
              <Button type="submit" variant="primary" loading={pending} disabled={pending}>
                Save text corrections (new revision)
              </Button>
              <Button type="button" disabled={pending} onClick={cancelEdit}>
                Cancel
              </Button>
            </div>
          </form>
        )}
        {!editing && savedNote && (
          <p className="text-label text-ok" role="status">
            {savedNote}
          </p>
        )}
      </div>
  );

  const approvalSection = doc !== null && (
        <section aria-label="Content approval" className="card px-4 py-4 grid gap-2">
          <h4 className="text-cardtitle font-medium text-ink-100">Approval — content-only</h4>
          <p className="text-label text-ink-300">
            {doc.approval.valid ? (
              <span className="chip" title="Content-only approval on this revision">
                Approved r{doc.approval.revision}
              </span>
            ) : (
              "No content approval on this revision. Any content edit invalidates approval."
            )}{" "}
            <span className="text-ink-500">
              Approval never sends — the bounded send below is a separate operator decision over
              exact revisions.
            </span>
          </p>
          {approveError && (
            <p className="text-label text-fail" role="alert">
              {approveError}
            </p>
          )}
          {canWrite && !doc.approval.valid && (
            <div>
              <Button
                size="sm"
                loading={approvePending}
                disabled={approvePending}
                onClick={() => {
                  setApprovePending(true);
                  setApproveError(null);
                  void contentClient
                    .approve(scope, campaignId, doc.revision)
                    .then((value) => onDoc(parseContentDoc(value)))
                    .catch((err: unknown) => setApproveError(friendlyCampaignError(err)))
                    .finally(() => setApprovePending(false));
                }}
              >
                Approve r{doc.revision} (content-only)
              </Button>
            </div>
          )}
        </section>
  );

  const senderSection = doc !== null && (
        <section aria-label="SMTP sender" className="card px-4 py-4 grid gap-2">
          <h4 className="text-cardtitle font-medium text-ink-100">Sender binding</h4>
          {binding === undefined && bindingError === null && (
            <p className="text-label text-ink-400" role="status">
              Reading the sender binding…
            </p>
          )}
          {bindingError !== null && (
            <p className="text-label text-fail" role="alert">
              {bindingError}{" "}
              <button
                type="button"
                className="lnk"
                onClick={() => {
                  setBinding(undefined);
                  setBindingError(null);
                  setBindingToken((count) => count + 1);
                }}
              >
                Retry
              </button>
            </p>
          )}
          {binding !== undefined && (
            <SenderBindPanel
              scope={scope}
              viewer={viewer}
              binding={binding}
              onBinding={setBinding}
            />
          )}
        </section>
  );

  const testSection = doc !== null && (
        <section aria-label="Test send" className="card px-4 py-4 grid gap-2 crm-test">
          <h4 className="text-cardtitle font-medium text-ink-100">
            Test send — one operator address, real SMTP
          </h4>
          <p className="text-label text-ink-400">
            Submits the exact frozen content bytes to the bound sender's SMTP server for one
            operator-typed address. The receipt records SMTP acceptance or refusal only — a
            campaign send needs one accepted test send of this exact content and binding.
          </p>
          {binding === null && bindingError === null && (
            <p className="text-label text-ink-500" data-testsend="disabled">
              No SMTP sender is bound — bind one above to send a test.
            </p>
          )}
          <form
            className="crm-field-row"
            onSubmit={(e) => {
              e.preventDefault();
              setTestError(null);
              setTestReceipt(null);
              if (testEmail.trim() === "") {
                setTestError("a test recipient address is required");
                return;
              }
              setTestPending(true);
              void sendClient
                .smtpTestSend(scope, campaignId, testEmail.trim())
                .then((value) => setTestReceipt(parseTestSendReceipt(value)))
                .catch((err: unknown) => setTestError(friendlySendError(err)))
                .finally(() => setTestPending(false));
            }}
          >
            <div className="crm-field">
              <label className="text-label text-ink-300" htmlFor="cmp-test-email">
                Test recipient (one operator address)
              </label>
              <input
                id="cmp-test-email"
                className="field"
                type="email"
                value={testEmail}
                onChange={(e) => setTestEmail(e.target.value)}
                maxLength={254}
                autoComplete="off"
                disabled={!canWrite || testPending || binding !== undefined && binding === null}
                placeholder="name@example.com"
              />
            </div>
            <div>
              <span className="text-label text-ink-300">Send</span>
              <div className="mt-1">
                <Button
                  type="submit"
                  variant="primary"
                  size="sm"
                  loading={testPending}
                  disabled={!canWrite || testPending || binding === null || binding === undefined}
                  title={
                    binding === null
                      ? "No SMTP sender is bound"
                      : "Submit one test message to the bound SMTP server"
                  }
                >
                  Send test
                </Button>
              </div>
            </div>
          </form>
          {testError && (
            <p className="text-label text-fail" role="alert">
              {testError}
            </p>
          )}
          {testReceipt && (
            <dl className="crm-detail" aria-label="Test-send receipt" data-testreceipt>
              <div>
                <dt>Result</dt>
                <dd>
                  <span
                    className="chip crm-send-chip"
                    data-state={testReceipt.accepted ? "accepted" : "failed"}
                  >
                    {testReceipt.accepted ? "Accepted" : "Refused"}
                  </span>{" "}
                  <span className="num">
                    SMTP {testReceipt.smtpCode ?? "—"} {testReceipt.smtpMessage}
                  </span>
                </dd>
              </div>
              <div>
                <dt>Recipient</dt>
                <dd className="num">{testReceipt.to}</dd>
              </div>
              <div>
                <dt>Content</dt>
                <dd className="num">
                  r{testReceipt.contentRevision} · {testReceipt.contentDigest.slice(0, 18)}…
                </dd>
              </div>
              <div>
                <dt>Claim</dt>
                <dd className="text-ink-300">
                  SMTP accepted the message — this is not proof of inbox delivery.
                </dd>
              </div>
            </dl>
          )}
        </section>
  );

  // CAD-1013: proposals stay available before revision 1 — the assistant
  // mint/apply path is the creation route now that the manual composer is
  // gone. expectedRevision is 0 for an unsaved campaign (source_revision=0).
  const proposalsSection = (
        <section aria-label="Assistant proposals" className="card px-4 py-4 grid gap-3">
          <h4 className="text-cardtitle font-medium text-ink-100">Proposals — Apply or Discard</h4>
          <p className="text-label text-ink-400">
            The left chat assistant drafts copy when the operator asks it to: mint one proposal
            request below, then its live turn answers with a host-verified draft. Only Apply
            changes the draft revision (approval invalidates); Discard is non-mutating. Nothing
            proposes, edits or sends silently.
          </p>
          {proposalsError && (
            <p className="text-label text-fail" role="alert">
              {proposalsError}{" "}
              <button type="button" className="lnk" onClick={() => setProposalToken((count) => count + 1)}>
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
          {canWrite && (
            <div className="grid gap-2" data-assistant-mint>
              <div className="crm-toolbar">
                <Button
                  size="sm"
                  variant="primary"
                  loading={mintPending}
                  disabled={mintPending || scopedChatMessage === null}
                  title={
                    scopedChatMessage === null
                      ? "Send the assistant a message in the left chat first"
                      : `Mint a one-time proposal request on chat message ${scopedChatMessage}`
                  }
                  onClick={mintRequest}
                >
                  Ask assistant to draft
                </Button>
                {scopedChatMessage === null && (
                  <span className="text-label text-ink-500" data-mint-hint>
                    Send the assistant a message in the left chat first
                  </span>
                )}
                {mintWatching && (
                  <span className="text-label text-ink-400" role="status" data-mint-watching>
                    Watching for the assistant's draft…
                  </span>
                )}
              </div>
              {minted !== null && (
                <p className="num text-micro text-ink-500" data-minted-request>
                  Request {minted.requestId} · campaign {minted.campaignId} · stamped source r
                  {minted.sourceRevision} · draft r{doc === null ? 0 : doc.revision} ·{" "}
                  {minted.state === "open" ? "awaiting the assistant's turn" : minted.state}
                  {minted.usedBy !== null ? ` by ${minted.usedBy}` : ""}
                </p>
              )}
            </div>
          )}
          {proposals.filter((row) => row.state === "pending").length === 0 ? (
            <p className="text-label text-ink-400" data-empty="proposals">
              No pending proposals for this campaign.
            </p>
          ) : (
            <ol className="crm-history" aria-label="Pending proposals">
              {proposals
                .filter((row) => row.state === "pending")
                .map((row) => (
                  <ProposalRow
                    key={row.proposalId}
                    scope={scope}
                    proposal={row}
                    expectedRevision={doc === null ? 0 : doc.revision}
                    canWrite={canWrite}
                    onApplied={(next) => {
                      onDoc(next);
                      setProposalToken((count) => count + 1);
                      setRender(null);
                      setTestReceipt(null);
                      setProposalNote(
                        `Applied as revision ${next.revision} — content approval invalidated; re-approve before any send preparation.`,
                      );
                    }}
                    onDiscarded={(id) => {
                      setProposalToken((count) => count + 1);
                      setProposalNote(
                        doc === null
                          ? `Proposal ${id} discarded — still no saved revision.`
                          : `Proposal ${id} discarded — draft unchanged at r${doc.revision} (${doc.contentDigest.slice(0, 18)}…).`,
                      );
                    }}
                    onError={setProposalError}
                  />
                ))}
            </ol>
          )}
        </section>
  );

  const finalSend = doc !== null && freezeId !== undefined && (
    <FinalSendPanel
      scope={scope}
      viewer={viewer}
      campaignId={campaignId}
      doc={doc}
      freezeId={freezeId}
      freeze={freeze ?? null}
      binding={binding}
      testEvidence={testReceipt}
    />
  );

  // New campaign (or a host without an audience slot): content first
  // with the always-mounted preview beside it; the audience picker
  // stays on the new page itself. Saved campaigns take the ordered
  // task layout below.
  if (audienceSlot === undefined) {
    return (
      <div className="grid gap-3">
        <div className="crm-compose">
          {contentForm}
          {previewSection}
        </div>
        {approvalSection}
        {senderSection}
        {testSection}
        {proposalsSection}
        {finalSend}
      </div>
    );
  }
  // CAD-1008 saved campaign task order: Content+Preview → content
  // approval → Audience/freeze → Sender → Test → Proposals → Final
  // send. Outcomes (sends list) render on the detail page after this.
  return (
    <div className="grid gap-3">
      <div className="crm-compose">
        {contentForm}
        {previewSection}
      </div>
      {approvalSection}
      {audienceSlot}
      {senderSection}
      {testSection}
      {proposalsSection}
      {finalSend}
    </div>
  );
}

/**
 * One pending proposal. The CAD-813 render rule is strict: only
 * `actor == "assistant"` AND a non-null `assistant_receipt` earns the
 * verified badge — an operator submission (or a receipt-less row) is
 * always labelled operator-submitted, never assistant, whatever its
 * text looks like. A pending row whose stamped source drifted behind
 * the current draft renders "Needs review (stale)" with Apply
 * disabled — the operator re-reviews instead of merging late output.
 */
function ProposalRow({
  scope,
  proposal,
  expectedRevision,
  canWrite,
  onApplied,
  onDiscarded,
  onError,
}: {
  scope: AudienceScope;
  proposal: ProposalDoc;
  expectedRevision: number;
  canWrite: boolean;
  onApplied: (doc: ContentDoc) => void;
  onDiscarded: (proposalId: string) => void;
  onError: (message: string | null) => void;
}) {
  const [pending, setPending] = useState<"apply" | "discard" | null>(null);
  const verified = proposal.actor === "assistant" && proposal.assistantReceipt !== null;
  const stale = proposal.sourceRevision !== expectedRevision;
  const receipt = proposal.assistantReceipt;
  return (
    <li className="card px-3 py-3" data-proposal={proposal.proposalId}>
      <p className="text-label text-ink-200">
        <span className="num">{proposal.proposalId}</span> · {proposal.subject}
      </p>
      {verified && receipt !== null ? (
        <p className="mt-1">
          <span
            className="chip"
            data-badge="verified-assistant"
            title="Host-verified: the daemon stamped this draft's agent, request, campaign and source revision — the browser's copy is never the authority"
          >
            Verified assistant draft
          </span>{" "}
          <span className="num text-micro text-ink-500">
            agent {receipt.agent} · request {receipt.requestId} · campaign {receipt.campaignId} ·
            source r{receipt.sourceRevision}
          </span>
        </p>
      ) : (
        <p className="mt-1">
          <span
            className="chip"
            data-badge="operator-submitted"
            title="Submitted through the operator proposal route — no assistant provenance"
          >
            Operator-submitted
          </span>{" "}
          <span className="num text-micro text-ink-500">
            actor {proposal.actor} · origin {proposal.origin} · source r{proposal.sourceRevision}
          </span>
        </p>
      )}
      {stale && proposal.state === "pending" && (
        <p className="text-label text-warn mt-1" data-state="stale">
          Needs review (stale) — stamped against source r{proposal.sourceRevision}, the draft is
          now r{expectedRevision}. Re-review its text before re-minting a request.
        </p>
      )}
      {canWrite && (
        <div className="crm-toolbar mt-2">
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
              void contentClient
                .proposalApply(scope, proposal.proposalId, expectedRevision)
                .then((value) => onApplied(parseContentDoc(value)))
                .catch((err: unknown) => onError(friendlyCampaignError(err)))
                .finally(() => setPending(null));
            }}
          >
            Apply (new revision)
          </Button>
          <Button
            size="sm"
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
        </div>
      )}
    </li>
  );
}

function SenderPanel({ render }: { render: ContentRender }) {
  return (
    <section aria-label="Sender" className="card px-3 py-3 grid gap-2">
      <h4 className="text-label font-medium text-ink-200">Sender in this render — preview</h4>
      <dl className="crm-detail">
        <div>
          <dt>From</dt>
          <dd className="num">
            {render.sender.name} · {render.sender.address}
          </dd>
        </div>
        <div>
          <dt>Unsubscribe</dt>
          <dd className="num">{render.unsubscribeUrl}</dd>
        </div>
        <div>
          <dt>Binding</dt>
          <dd className="num">
            {render.bindingId} · preview-only
            {render.previewOnly ? "" : " (unexpected — reported)"}
          </dd>
        </div>
      </dl>
      <p className="text-micro text-ink-500">
        The render names the host's preview sender; the real sender is the bound SMTP connection
        above, and every send's unsubscribe links build on the configured origin. A preview proves
        the bytes only — sending is the operator's separate decision below.
      </p>
    </section>
  );
}

/* ------------------------------------------------------------------ */
/* New + detail pages.                                                 */
/* ------------------------------------------------------------------ */

function CampaignNew({
  scope,
  scopedChatMessage,
  viewer,
  onCreated,
  onCancel,
}: {
  scope: AudienceScope;
  scopedChatMessage: string | null;
  viewer: Viewer;
  onCreated: (campaignId: string) => void;
  onCancel: () => void;
}) {
  const headRef = useRef<HTMLHeadingElement | null>(null);
  const [campaignId, setCampaignId] = useState(() => newAudienceId("cmp"));
  const [pick, setPick] = useState<AudiencePick>({ base: { mode: "all" }, exclusionListId: null });
  const [, setAudiencePreview] = useState<AudiencePreview | null>(null);
  const [doc, setDoc] = useState<ContentDoc | null>(null);
  const [freezeId, setFreezeId] = useState("");
  const [ceiling, setCeiling] = useState("500");
  const [freezePending, setFreezePending] = useState(false);
  const [freezeError, setFreezeError] = useState<string | null>(null);
  const [freezeDone, setFreezeDone] = useState<string | null>(null);
  useEffect(() => {
    headRef.current?.focus();
  }, []);
  useEffect(() => {
    setFreezeId((prev) => (prev === "" ? `${campaignId}-freeze-1` : prev));
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [campaignId]);
  return (
    <section aria-label="New campaign" className="grid gap-3">
      <div>
        <h3 ref={headRef} className="text-cardtitle font-medium text-ink-100" tabIndex={-1} data-outlet-heading>
          New campaign
        </h3>
        <p className="text-label text-ink-400 mt-1">
          <button type="button" className="lnk" onClick={onCancel}>
            ← Campaigns
          </button>{" "}
          — audience previews need no save; the email preview, test send and proposals unlock
          after the first save.
        </p>
      </div>
      {!viewer.operator ? (
        <p className="card px-4 py-3 text-label text-ink-400">
          Sign in as the operator to create campaigns.
        </p>
      ) : viewer.readOnly ? (
        <p className="card px-4 py-3 text-label text-ink-400" data-state="read-only">
          Read-only view. A verified operator creates campaigns.
        </p>
      ) : scope.contextId === "" ? (
        <p className="card px-4 py-3 text-label text-ink-400">
          Administrator CRM setup is required before creating a campaign.
        </p>
      ) : (
        <>
          <Field
            label="Campaign ID"
            id="cmp-id"
            hint="Letters, digits, - _"
            disabled={doc !== null}
            className="crm-field"
          >
            {(c) => (
              <input
                {...c}
                className="field"
                value={campaignId}
                onChange={(e) => setCampaignId(e.target.value)}
                maxLength={128}
                autoComplete="off"
              />
            )}
          </Field>
          <AudienceSection
            scope={scope}
            viewer={viewer}
            pick={pick}
            onPick={setPick}
            onPreview={setAudiencePreview}
            freezeSlot={
              <section aria-label="Freeze audience" className="grid gap-2">
                <h4 className="text-label font-medium text-ink-200">
                  Freeze this audience (optional, named by the operator)
                </h4>
                <div className="crm-field-row">
                  <div className="crm-field">
                    <label className="text-label text-ink-300" htmlFor="cmp-freeze-id">
                      Freeze ID
                    </label>
                    <input
                      id="cmp-freeze-id"
                      className="field"
                      value={freezeId}
                      onChange={(e) => setFreezeId(e.target.value)}
                      maxLength={128}
                      autoComplete="off"
                      disabled={freezePending}
                    />
                  </div>
                  <div className="crm-field">
                    <label className="text-label text-ink-300" htmlFor="cmp-ceiling">
                      Recipient ceiling (1–500)
                    </label>
                    <input
                      id="cmp-ceiling"
                      className="field num"
                      inputMode="numeric"
                      value={ceiling}
                      onChange={(e) => setCeiling(e.target.value)}
                      maxLength={4}
                      autoComplete="off"
                      disabled={freezePending}
                    />
                  </div>
                </div>
                {freezeError && (
                  <p className="text-label text-fail" role="alert">
                    {freezeError}
                  </p>
                )}
                {freezeDone && (
                  <p className="text-label text-ok" role="status">
                    {freezeDone}
                  </p>
                )}
                <div>
                  <Button
                    size="sm"
                    loading={freezePending}
                    disabled={freezePending}
                    onClick={() => {
                      setFreezeError(null);
                      setFreezeDone(null);
                      const max = Number(ceiling);
                      if (freezeId.trim() === "" || !Number.isInteger(max) || max < 1 || max > 500) {
                        setFreezeError("a freeze needs an ID and a ceiling of 1 to 500");
                        return;
                      }
                      if (pick.base.mode === "custom" && pick.base.customerIds.length === 0) {
                        setFreezeError("a custom base needs at least one customer ID");
                        return;
                      }
                      setFreezePending(true);
                      void audienceClient
                        .prepare(scope, {
                          freezeId: freezeId.trim(),
                          base: pick.base,
                          exclusionListId: pick.exclusionListId ?? undefined,
                          maxRecipients: max,
                        })
                        .then((value) => {
                          const freeze = (value as { freeze?: Record<string, unknown> } | null)?.freeze;
                          const count = (freeze?.final_count as number | undefined) ?? -1;
                          setFreezeDone(
                            `Frozen ${freezeId.trim()} with ${count} recipients — recheck validity on the campaign detail.`,
                          );
                        })
                        .catch((err: unknown) => setFreezeError(friendlyAudienceError(err)))
                        .finally(() => setFreezePending(false));
                    }}
                  >
                    Freeze audience
                  </Button>
                </div>
              </section>
            }
          />
          <CampaignWorkspace
            scope={scope}
            scopedChatMessage={scopedChatMessage}
            viewer={viewer}
            campaignId={campaignId}
            doc={doc}
            onDoc={setDoc}
          />
          {doc !== null && (
            <p className="card px-4 py-3 text-label text-ink-300">
              Saved revision r{doc.revision}.{" "}
              <button type="button" className="lnk" onClick={() => onCreated(doc.campaignId)}>
                Open campaign detail →
              </button>
            </p>
          )}
        </>
      )}
    </section>
  );
}

function CampaignDetail({
  scope,
  scopedChatMessage,
  viewer,
  campaignId,
  onBack,
}: {
  scope: AudienceScope;
  scopedChatMessage: string | null;
  viewer: Viewer;
  campaignId: string;
  onBack: () => void;
}) {
  const headRef = useRef<HTMLHeadingElement | null>(null);
  const [doc, setDoc] = useState<ContentDoc | null>(null);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);
  const [pick, setPick] = useState<AudiencePick>({ base: { mode: "all" }, exclusionListId: null });
  const [, setAudiencePreview] = useState<AudiencePreview | null>(null);
  const [freezeId, setFreezeId] = useState(`${campaignId}-freeze-1`);
  const [freeze, setFreeze] = useState<{
    finalCount: number;
    digest: string;
    valid: boolean | null;
    drift: string | null;
    currentCount: number | null;
  } | null>(null);
  const [freezePending, setFreezePending] = useState(false);
  const [freezeError, setFreezeError] = useState<string | null>(null);

  useEffect(() => {
    headRef.current?.focus();
  }, []);

  const reloadToken = `${scope.installId}:${scope.contextId}:${campaignId}`;
  useEffect(() => {
    const controller = new AbortController();
    setLoading(true);
    setError(null);
    contentClient
      .show(scope, campaignId)
      .then((value) => {
        if (!controller.signal.aborted) setDoc(parseContentDoc(value));
      })
      .catch((e: unknown) => {
        if (!controller.signal.aborted) setError(friendlyCampaignError(e));
      })
      .finally(() => {
        if (!controller.signal.aborted) setLoading(false);
      });
    return () => controller.abort();
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [reloadToken]);

  const checkFreeze = () => {
    if (freezeId.trim() === "") {
      setFreezeError("a freeze ID is required to recheck validity");
      return;
    }
    setFreezePending(true);
    setFreezeError(null);
    void audienceClient
      .freezeShow(scope, freezeId.trim())
      .then((value) => {
        const root = (value as Record<string, unknown> | null) ?? {};
        const row = (root.freeze as Record<string, unknown> | null) ?? {};
        setFreeze({
          finalCount: typeof row.final_count === "number" ? row.final_count : -1,
          digest: typeof row.digest === "string" ? row.digest : "",
          valid: typeof root.valid === "boolean" ? root.valid : null,
          drift: typeof root.drift === "string" ? root.drift : null,
          currentCount: typeof root.current_final_count === "number" ? root.current_final_count : null,
        });
      })
      .catch((err: unknown) => setFreezeError(friendlyAudienceError(err)))
      .finally(() => setFreezePending(false));
  };

  return (
    <section aria-label="Campaign details" className="grid gap-3">
      <div>
        <h3 ref={headRef} className="text-cardtitle font-medium text-ink-100" tabIndex={-1} data-outlet-heading>
          {loading ? "Campaign details" : (doc?.subject ?? "Campaign details")}
        </h3>
        <p className="text-label text-ink-400 mt-1">
          <button type="button" className="lnk" onClick={onBack}>
            ← Campaigns
          </button>{" "}
          <span className="num">· {campaignId}</span>
        </p>
        <details className="crm-diag">
          <summary className="text-micro text-ink-500">Record diagnostics</summary>
          <p className="num text-micro text-ink-500 mt-1">
            Campaign <span className="num">{campaignId}</span> · workspace{" "}
            <span className="num">{scope.contextId || "none"}</span>
            {doc !== null && (
              <>
                {" "}· revision r{doc.revision} · digest {doc.contentDigest.slice(0, 18)}…
              </>
            )}
          </p>
        </details>
      </div>
      {loading && (
        <p className="text-secondary text-ink-400" role="status">
          Reading the campaign…
        </p>
      )}
      {error !== null && !loading && (
        <p className="card px-4 py-3 text-label text-fail border-fail/40" role="alert">
          {error}{" "}
          <button
            type="button"
            className="lnk"
            onClick={() => {
              setError(null);
              setLoading(true);
              contentClient
                .show(scope, campaignId)
                .then((value) => setDoc(parseContentDoc(value)))
                .catch((e: unknown) => setError(friendlyCampaignError(e)))
                .finally(() => setLoading(false));
            }}
          >
            Retry
          </button>
        </p>
      )}
      {!loading && error === null && doc !== null && (
        <>
          <CampaignWorkspace
            scope={scope}
            scopedChatMessage={scopedChatMessage}
            viewer={viewer}
            campaignId={campaignId}
            doc={doc}
            onDoc={setDoc}
            freezeId={freezeId}
            freeze={freeze}
            audienceSlot={
              <section aria-label="Frozen audience" className="card px-4 py-4 grid gap-3">
            <h4 className="text-cardtitle font-medium text-ink-100">Audience &amp; freeze</h4>
            <p className="text-label text-ink-400">
              Freezes are named workspace rows the operator addresses by ID — the host stores no
              campaign-to-audience link, so this panel names the freeze explicitly (default{" "}
              <span className="num">{campaignId}-freeze-1</span>) and rechecks its live validity.
              Any segment, exclusion, consent or suppression drift reports invalid.
            </p>
            <AudienceSection
              scope={scope}
              viewer={viewer}
              pick={pick}
              onPick={setPick}
              onPreview={setAudiencePreview}
            />
            <div className="crm-field-row">
              <div className="crm-field">
                <label className="text-label text-ink-300" htmlFor="cmp-detail-freeze">
                  Freeze ID to recheck
                </label>
                <input
                  id="cmp-detail-freeze"
                  className="field"
                  value={freezeId}
                  onChange={(e) => setFreezeId(e.target.value)}
                  maxLength={128}
                  autoComplete="off"
                  disabled={freezePending}
                />
              </div>
              <div>
                <span className="text-label text-ink-300">Validity</span>
                <div className="mt-1">
                  <Button size="sm" loading={freezePending} disabled={freezePending} onClick={checkFreeze}>
                    Recheck freeze
                  </Button>
                </div>
              </div>
            </div>
            {freezeError && (
              <p className="text-label text-fail" role="alert">
                {freezeError}
              </p>
            )}
            {freeze && (
              <dl className="crm-detail" aria-label="Freeze validity">
                <div>
                  <dt>Frozen recipients</dt>
                  <dd className="num">{freeze.finalCount}</dd>
                </div>
                <div>
                  <dt>Current recount</dt>
                  <dd className="num">{freeze.currentCount ?? "—"}</dd>
                </div>
                <div>
                  <dt>Validity</dt>
                  <dd>
                    <span
                      className="chip"
                      title={freeze.drift ?? "The frozen digest still matches the live audience"}
                    >
                      {freeze.valid === true ? "Valid" : freeze.valid === false ? `Invalid — ${freeze.drift}` : "Unknown"}
                    </span>
                  </dd>
                </div>
                <div>
                  <dt>Digest</dt>
                  <dd className="num" title="Frozen audience digest">
                    {freeze.digest.slice(0, 18)}…
                  </dd>
                </div>
              </dl>
            )}
              </section>
            }
          />
          <CampaignSends scope={scope} viewer={viewer} campaignId={campaignId} />
        </>
      )}
    </section>
  );
}

/** Prior sends of this campaign — the same `list` read the campaigns
 *  table shows, plus each send's live counts on demand. */
function CampaignSends({
  scope,
  viewer,
  campaignId,
}: {
  scope: AudienceScope;
  viewer: Viewer;
  campaignId: string;
}) {
  const [sends, setSends] = useState<SendListEntry[] | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [openId, setOpenId] = useState<string | null>(null);

  const token = `${scope.installId}:${scope.contextId}:${campaignId}`;
  useEffect(() => {
    const controller = new AbortController();
    sendClient
      .sendList(scope, campaignId)
      .then((value) => {
        if (!controller.signal.aborted) {
          setSends(parseSendList(value));
          setError(null);
        }
      })
      .catch((e: unknown) => {
        if (!controller.signal.aborted) setError(friendlySendError(e));
      });
    return () => controller.abort();
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [token]);

  return (
    <section aria-label="Campaign sends" className="card px-4 py-4 grid gap-3">
      <h4 className="text-cardtitle font-medium text-ink-100">Sends of this campaign</h4>
      {error !== null ? (
        <p className="text-label text-fail" role="alert">
          {error}
        </p>
      ) : sends === null ? (
        <p className="text-label text-ink-400" role="status">
          Reading sends…
        </p>
      ) : sends.length === 0 ? (
        <p className="text-label text-ink-400" data-empty="sends">
          No sends of this campaign yet — prepare and approve above.
        </p>
      ) : (
        <ol className="crm-history" aria-label="Sends">
          {sends.map((row) => {
            const total =
              row.counts.queued +
              row.counts.submitting +
              row.counts.accepted +
              row.counts.failed +
              row.counts.uncertain +
              row.counts.suppressed +
              row.counts.closed;
            return (
              <li key={row.sendId} className="card px-3 py-3">
                <p className="text-label text-ink-200">
                  <span className="num">{row.sendId}</span>{" "}
                  <span className="chip crm-send-chip" data-state={row.state}>
                    {row.state}
                  </span>{" "}
                  <span className="num text-ink-400">
                    {row.counts.accepted}/{total} accepted · digest {row.sendDigest.slice(0, 18)}…
                  </span>{" "}
                  <button
                    type="button"
                    className="lnk"
                    onClick={() => setOpenId(openId === row.sendId ? null : row.sendId)}
                  >
                    {openId === row.sendId ? "Hide" : "Show"}
                  </button>
                </p>
                {openId === row.sendId && (
                  <div className="mt-2">
                    <SendProgressPanel scope={scope} viewer={viewer} sendId={row.sendId} />
                  </div>
                )}
              </li>
            );
          })}
        </ol>
      )}
    </section>
  );
}

export type { AudienceScope };
