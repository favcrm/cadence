import { useEffect, useRef, useState } from "react";
import { ApiError } from "../../lib/api";
import Button from "../../ui/Button";
import Link from "../../ui/Link";
import Select from "../../ui/Select";
import type { Viewer } from "../projects/work";
import {
  audienceClient,
  type AudienceBaseInput,
  type AudienceScope,
} from "./audienceClient";
import {
  BLOCK_TYPES,
  checkCampaignId,
  checkContent,
  friendlyCampaignError,
  isBlockType,
  parseContentDoc,
  parseContentList,
  parseProposal,
  parseProposalList,
  parseRender,
  withToken,
  type CampaignBlock,
  type ContentDoc,
  type ContentRender,
  type ProposalDoc,
} from "./campaignGrammar";
import { contentClient } from "./contentClient";
import { PreviewPanel, parsePreview, type AudiencePreview } from "./CrmSegments";
import { friendlyAudienceError, newAudienceId } from "./segmentGrammar";

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
 * - Sender and unsubscribe material is host-locked preview-only
 *   bytes; final-send preparation refuses until CAD-785/786 supply
 *   verified authority. The Send control stays disabled and labelled.
 * - Proposals submitted here are operator-attributed
 *   (`operator-direct`, no assistant receipt): the receipt-backed
 *   assistant seam does not exist yet, so left-chat suggestions only
 *   land through an explicit Save or Submit press — never silently.
 */

export default function CrmCampaigns({
  scope,
  viewer,
  view,
  recordId,
  onView,
  onSelect,
  onRecordCreated,
}: {
  scope: AudienceScope;
  viewer: Viewer;
  view: "list" | "new";
  recordId: string | null;
  onView: (view: "list" | "new") => void;
  onSelect: (recordId: string | null) => void;
  onRecordCreated?: (recordId: string) => void;
}) {
  return (
    <div className="crm-list" data-section="campaigns">
      <nav className="crm-crumb" aria-label="Breadcrumb">
        <Link href="/apps" className="lnk text-label">
          Apps
        </Link>
        <span aria-hidden="true" className="text-ink-600">
          /
        </span>
        <span className="text-label text-ink-300">CRM</span>
        <span aria-hidden="true" className="text-ink-600">
          /
        </span>
        <span className="text-label text-ink-100" aria-current="page">
          Campaigns{view === "new" ? " / New" : ""}
          {recordId !== null ? " / Details" : ""}
        </span>
      </nav>

      {recordId !== null ? (
        <CampaignDetail
          scope={scope}
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

  return (
    <section aria-label="Campaigns list">
      <div className="crm-toolbar">
        <p className="text-secondary text-ink-300">
          Versioned email content with content-only approval. Audience freezes and test-send
          receipts live on each campaign's detail page — the host stores no campaign-level
          audience link beyond the named freeze.
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
          Pick an App context above to list its campaigns.
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
          <p className="font-medium text-ink-200">No campaigns yet in this context</p>
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
                <th scope="col">Campaign</th>
                <th scope="col">Subject</th>
                <th scope="col">Rev</th>
                <th scope="col">Status</th>
                <th scope="col">
                  <span className="sr-only">Open</span>
                </th>
              </tr>
            </thead>
            <tbody>
              {campaigns.map((campaign) => (
                <tr key={campaign.campaignId}>
                  <td className="num text-ink-100">{campaign.campaignId}</td>
                  <td className="text-ink-300">{campaign.subject}</td>
                  <td className="num text-ink-500">r{campaign.revision}</td>
                  <td>
                    <span
                      className="chip"
                      title={campaign.approval.valid ? "Content-only approval on this revision" : "No content approval on this revision"}
                    >
                      {campaign.approval.valid ? `Approved r${campaign.approval.revision}` : "Draft"}
                    </span>
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

interface TestReceipt {
  to: string;
  revision: number;
  contentDigest: string;
  payloadDigest: string;
}

function parseTestPrepare(value: unknown): TestReceipt {
  const send = (value as { test_send?: unknown } | null)?.test_send;
  if (!send || typeof send !== "object") {
    throw new ApiError("The server returned an invalid test-send receipt", 502);
  }
  const row = send as Record<string, unknown>;
  if (
    typeof row.to_email !== "string" ||
    typeof row.content_revision !== "number" ||
    typeof row.content_digest !== "string" ||
    typeof row.payload_digest !== "string"
  ) {
    throw new ApiError("The server returned an invalid test-send receipt", 502);
  }
  return {
    to: row.to_email,
    revision: row.content_revision,
    contentDigest: row.content_digest,
    payloadDigest: row.payload_digest,
  };
}

function CampaignWorkspace({
  scope,
  viewer,
  campaignId,
  doc,
  onDoc,
}: {
  scope: AudienceScope;
  viewer: Viewer;
  campaignId: string;
  doc: ContentDoc | null;
  onDoc: (doc: ContentDoc) => void;
}) {
  const canWrite = viewer.operator && !viewer.readOnly;
  const [subject, setSubject] = useState(doc?.subject ?? "");
  const [preheader, setPreheader] = useState(doc?.preheader ?? "");
  const [blocks, setBlocks] = useState<EditorBlock[]>(() => blocksFromDoc(doc));
  const [fallback, setFallback] = useState("Friend");
  const [pending, setPending] = useState(false);
  const [formError, setFormError] = useState<string | null>(null);
  const [savedNote, setSavedNote] = useState<string | null>(null);
  const [render, setRender] = useState<ContentRender | null>(null);
  const [renderPending, setRenderPending] = useState(false);
  const [renderError, setRenderError] = useState<string | null>(null);
  const [previewTab, setPreviewTab] = useState<"visual" | "html" | "text">("visual");
  const [sampleName, setSampleName] = useState("");
  const [approvePending, setApprovePending] = useState(false);
  const [approveError, setApproveError] = useState<string | null>(null);
  const [testEmail, setTestEmail] = useState("");
  const [testPending, setTestPending] = useState(false);
  const [testError, setTestError] = useState<string | null>(null);
  const [testReceipt, setTestReceipt] = useState<TestReceipt | null>(null);
  const [proposals, setProposals] = useState<ProposalDoc[]>([]);
  const [proposalsError, setProposalsError] = useState<string | null>(null);
  const [proposalToken, setProposalToken] = useState(0);
  const [proposalPending, setProposalPending] = useState(false);
  const [proposalError, setProposalError] = useState<string | null>(null);
  const [proposalNote, setProposalNote] = useState<string | null>(null);

  // The doc is the saved truth: editor follows a newly saved revision
  // (create, Apply) but never clobbers typing mid-draft.
  const docIdentity = doc === null ? "none" : `${doc.revision}:${doc.contentDigest}`;
  useEffect(() => {
    if (doc === null) return;
    setSubject(doc.subject);
    setPreheader(doc.preheader);
    setBlocks(blocksFromDoc(doc));
    setRender(null);
    setTestReceipt(null);
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [docIdentity]);

  const proposalsKey = `${scope.installId}:${scope.contextId}:${campaignId}:${proposalToken}`;
  useEffect(() => {
    if (doc === null || !viewer.operator) {
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
        setSavedNote(
          doc === null
            ? `Created revision ${next.revision} — earlier content approval does not exist yet.`
            : `Saved revision ${next.revision} — content approval invalidated.`,
        );
      })
      .catch((err: unknown) => setFormError(friendlyCampaignError(err)))
      .finally(() => setPending(false));
  };

  const preview = () => {
    if (doc === null) return;
    setRenderPending(true);
    setRenderError(null);
    void contentClient
      .render(scope, campaignId, sampleName.trim() === "" ? undefined : { sampleFirstName: sampleName.trim() })
      .then((value) => setRender(parseRender(value)))
      .catch((err: unknown) => setRenderError(friendlyCampaignError(err)))
      .finally(() => setRenderPending(false));
  };

  const updateBlock = (key: number, patch: Partial<EditorBlock>) => {
    setBlocks((prev) => prev.map((block) => (block.key === key ? { ...block, ...patch } : block)));
  };

  return (
    <div className="grid gap-3">
      <form className="card px-4 py-4 grid gap-3" aria-label="Email content" onSubmit={save}>
        <h4 className="text-cardtitle font-medium text-ink-100">
          Content {doc === null ? "— unsaved draft" : `— revision r${doc.revision}`}
        </h4>
        <div className="crm-field-row">
          <div className="crm-field">
            <label className="text-label text-ink-300" htmlFor="cmp-subject">
              Subject (required, plain text)
            </label>
            <input
              id="cmp-subject"
              className="field"
              value={subject}
              onChange={(e) => setSubject(e.target.value)}
              maxLength={150}
              autoComplete="off"
              disabled={!canWrite || pending}
              required
            />
          </div>
          <div className="crm-field">
            <label className="text-label text-ink-300" htmlFor="cmp-preheader">
              Preheader (optional, plain text)
            </label>
            <input
              id="cmp-preheader"
              className="field"
              value={preheader}
              onChange={(e) => setPreheader(e.target.value)}
              maxLength={200}
              autoComplete="off"
              disabled={!canWrite || pending}
            />
          </div>
        </div>
        <div className="crm-field-row">
          <div className="crm-field">
            <label className="text-label text-ink-300" htmlFor="cmp-fallback">
              First-name fallback for the token
            </label>
            <input
              id="cmp-fallback"
              className="field"
              value={fallback}
              onChange={(e) => setFallback(e.target.value)}
              maxLength={40}
              autoComplete="off"
              disabled={!canWrite || pending}
            />
          </div>
          <p className="text-micro text-ink-500">
            The only approved personalization is {"{{first_name|Fallback}}"}. Paste carrying HTML,
            scripts or other merge fields is refused and nothing is mutated.
          </p>
        </div>
        <ol className="crm-history" aria-label="Content blocks">
          {blocks.map((block, index) => (
            <li key={block.key} className="card px-3 py-3">
              <div className="crm-field-row">
                <div className="crm-field">
                  <label className="text-label text-ink-300" htmlFor={`cmp-block-type-${block.key}`}>
                    Block {index + 1} type
                  </label>
                  <Select
                    id={`cmp-block-type-${block.key}`}
                    value={block.kind}
                    onChange={(value) =>
                      isBlockType(value) && updateBlock(block.key, { kind: value })
                    }
                    options={BLOCK_TYPES.map((entry) => ({ value: entry.value, label: entry.label }))}
                    aria-label={`Block ${index + 1} type`}
                    disabled={!canWrite || pending}
                    full
                  />
                </div>
                {block.kind === "button" ? (
                  <div className="crm-field">
                    <label className="text-label text-ink-300" htmlFor={`cmp-block-label-${block.key}`}>
                      Button label
                    </label>
                    <input
                      id={`cmp-block-label-${block.key}`}
                      className="field"
                      value={block.label}
                      onChange={(e) => updateBlock(block.key, { label: e.target.value })}
                      maxLength={60}
                      autoComplete="off"
                      disabled={!canWrite || pending}
                    />
                  </div>
                ) : (
                  <div className="crm-field">
                    <span className="text-label text-ink-300" id={`cmp-block-token-${block.key}`}>
                      First-name token
                    </span>
                    <div>
                      <Button
                        type="button"
                        size="sm"
                        disabled={!canWrite || pending}
                        aria-labelledby={`cmp-block-token-${block.key}`}
                        title="Append {{first_name|Fallback}} to this block"
                        onClick={() =>
                          updateBlock(block.key, { text: withToken(block.text, fallback) })
                        }
                      >
                        + {"{{first_name}}"}
                      </Button>
                      <Button
                        type="button"
                        size="sm"
                        disabled={!canWrite || pending}
                        title="Append {{first_name|Fallback}} to the subject"
                        onClick={() => setSubject((prev) => withToken(prev, fallback))}
                      >
                        + subject
                      </Button>
                    </div>
                  </div>
                )}
              </div>
              {block.kind === "button" ? (
                <div className="crm-field mt-2">
                  <label className="text-label text-ink-300" htmlFor={`cmp-block-url-${block.key}`}>
                    Button URL (https only)
                  </label>
                  <input
                    id={`cmp-block-url-${block.key}`}
                    className="field"
                    value={block.url}
                    onChange={(e) => updateBlock(block.key, { url: e.target.value })}
                    maxLength={500}
                    autoComplete="off"
                    disabled={!canWrite || pending}
                    placeholder="https://example.com/offer"
                  />
                </div>
              ) : (
                <div className="crm-field mt-2">
                  <label className="text-label text-ink-300" htmlFor={`cmp-block-text-${block.key}`}>
                    {block.kind === "heading" ? "Heading text" : "Paragraph text"}
                  </label>
                  <textarea
                    id={`cmp-block-text-${block.key}`}
                    className="field"
                    rows={block.kind === "heading" ? 2 : 4}
                    value={block.text}
                    onChange={(e) => updateBlock(block.key, { text: e.target.value })}
                    maxLength={block.kind === "heading" ? 120 : 2000}
                    autoComplete="off"
                    disabled={!canWrite || pending}
                  />
                </div>
              )}
              {blocks.length > 1 && canWrite && (
                <p className="mt-2">
                  <button
                    type="button"
                    className="lnk text-label"
                    disabled={pending}
                    onClick={() => setBlocks((prev) => prev.filter((row) => row.key !== block.key))}
                  >
                    Remove block {index + 1}
                  </button>
                </p>
              )}
            </li>
          ))}
        </ol>
        {canWrite && blocks.length < 12 && (
          <p className="crm-toolbar" aria-label="Add block">
            {BLOCK_TYPES.map((entry) => (
              <Button
                key={entry.value}
                type="button"
                size="sm"
                disabled={pending}
                onClick={() =>
                  isBlockType(entry.value) &&
                  setBlocks((prev) => [
                    ...prev,
                    { key: editorKey++, kind: entry.value, text: "", label: "", url: "" },
                  ])
                }
              >
                + {entry.label}
              </Button>
            ))}
            <span className="num text-micro text-ink-500">{blocks.length}/12</span>
          </p>
        )}
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
        {!canWrite ? (
          <p className="text-label text-ink-400" data-state="read-only">
            Read-only view. A verified operator saves content revisions.
          </p>
        ) : (
          <div>
            <Button type="submit" variant="primary" loading={pending} disabled={pending}>
              {doc === null ? "Create campaign (revision 1)" : `Save as r${doc.revision + 1}`}
            </Button>
          </div>
        )}
      </form>

      {doc !== null && (
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
              Approval never authorizes sending — delivery stays locked until CAD-785/786.
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
      )}

      {doc !== null && (
        <section aria-label="Email preview" className="card px-4 py-4 grid gap-3">
          <h4 className="text-cardtitle font-medium text-ink-100">
            Preview — host-rendered revision r{render?.revision ?? doc.revision}
          </h4>
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
              <span className="text-label text-ink-300">Host render</span>
              <div className="mt-1">
                <Button size="sm" loading={renderPending} disabled={renderPending} onClick={preview}>
                  Render preview
                </Button>
              </div>
            </div>
          </div>
          {renderError && (
            <p className="text-label text-fail" role="alert">
              {renderError}
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
                  title={`Visual email preview, revision ${render.revision}`}
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
        </section>
      )}

      {doc !== null && (
        <section aria-label="Test send" className="card px-4 py-4 grid gap-2 crm-test">
          <h4 className="text-cardtitle font-medium text-ink-100">Test send — prepared only</h4>
          <p className="text-label text-ink-400">
            Builds the exact send payload for one address so the operator can inspect it. Nothing
            is submitted to SMTP in this slice.
          </p>
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
              void contentClient
                .testPrepare(scope, campaignId, testEmail.trim())
                .then((value) => setTestReceipt(parseTestPrepare(value)))
                .catch((err: unknown) => setTestError(friendlyCampaignError(err)))
                .finally(() => setTestPending(false));
            }}
          >
            <div className="crm-field">
              <label className="text-label text-ink-300" htmlFor="cmp-test-email">
                Test recipient
              </label>
              <input
                id="cmp-test-email"
                className="field"
                type="email"
                value={testEmail}
                onChange={(e) => setTestEmail(e.target.value)}
                maxLength={254}
                autoComplete="off"
                disabled={!canWrite || testPending}
                placeholder="name@example.com"
              />
            </div>
            <div>
              <span className="text-label text-ink-300">Prepare</span>
              <div className="mt-1">
                <Button
                  type="submit"
                  variant="primary"
                  size="sm"
                  loading={testPending}
                  disabled={!canWrite || testPending}
                >
                  Prepare test send
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
            <dl className="crm-detail" aria-label="Test-send receipt">
              <div>
                <dt>Recipient</dt>
                <dd className="num">{testReceipt.to}</dd>
              </div>
              <div>
                <dt>Content</dt>
                <dd className="num">
                  r{testReceipt.revision} · {testReceipt.contentDigest.slice(0, 18)}…
                </dd>
              </div>
              <div>
                <dt>Payload</dt>
                <dd className="num" title="Send payload digest">
                  {testReceipt.payloadDigest.slice(0, 18)}… · preview-only, no SMTP
                </dd>
              </div>
            </dl>
          )}
        </section>
      )}

      {doc !== null && (
        <section aria-label="Assistant proposals" className="card px-4 py-4 grid gap-3">
          <h4 className="text-cardtitle font-medium text-ink-100">Proposals — Apply or Discard</h4>
          <p className="text-label text-ink-400">
            The left chat can suggest copy, but suggestions only land through an explicit Submit
            here, and only Apply changes the draft revision (approval invalidates). Discard is
            non-mutating. Nothing proposes, edits or sends silently.
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
            <div>
              <Button
                size="sm"
                loading={proposalPending}
                disabled={proposalPending}
                onClick={() => {
                  setProposalError(null);
                  setProposalNote(null);
                  let grammar: CampaignBlock[];
                  try {
                    grammar = grammarBlocks();
                    checkContent(subject, preheader, grammar);
                  } catch (err: unknown) {
                    setProposalError(friendlyCampaignError(err));
                    return;
                  }
                  setProposalPending(true);
                  void contentClient
                    .propose(scope, {
                      campaignId,
                      proposalId: newAudienceId("prop"),
                      subject,
                      preheader,
                      blocks: grammar,
                    })
                    .then((value) => {
                      const created = parseProposal(value);
                      setProposalToken((count) => count + 1);
                      setProposalNote(
                        `Proposal ${created.proposalId} submitted against r${created.sourceRevision} — still inert until Apply.`,
                      );
                    })
                    .catch((err: unknown) => setProposalError(friendlyCampaignError(err)))
                    .finally(() => setProposalPending(false));
                }}
              >
                Submit editor as proposal
              </Button>
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
                    expectedRevision={doc.revision}
                    canWrite={canWrite}
                    onApplied={(next) => {
                      onDoc(next);
                      setProposalToken((count) => count + 1);
                      setProposalNote(
                        `Applied as revision ${next.revision} — content approval invalidated.`,
                      );
                    }}
                    onDiscarded={(id) => {
                      setProposalToken((count) => count + 1);
                      setProposalNote(`Proposal ${id} discarded — draft unchanged at r${doc.revision}.`);
                    }}
                    onError={setProposalError}
                  />
                ))}
            </ol>
          )}
        </section>
      )}
    </div>
  );
}

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
  return (
    <li className="card px-3 py-3">
      <p className="text-label text-ink-200">
        <span className="num">{proposal.proposalId}</span> · {proposal.subject}
      </p>
      <p className="num text-micro text-ink-500 mt-1">
        actor {proposal.actor} · origin {proposal.origin} · source r{proposal.sourceRevision} ·
        assistant receipt {proposal.assistantReceipt === null ? "none (operator-submitted)" : "present"}
      </p>
      {canWrite && (
        <div className="crm-toolbar mt-2">
          <Button
            size="sm"
            variant="primary"
            loading={pending === "apply"}
            disabled={pending !== null}
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
      <h4 className="text-label font-medium text-ink-200">Sender — host-locked, preview-only</h4>
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
        Sender identity and unsubscribe footer are host-provided material until CAD-785/786 supply
        verified sender and unsubscribe authority. Final-send preparation refuses; the control
        below stays disabled and labelled for that reason.
      </p>
      <div>
        <Button
          size="sm"
          variant="primary"
          disabled
          title="Final send is locked until CAD-785 supplies a verified sender connection and CAD-786 supplies unsubscribe authority"
        >
          Send campaign (locked until CAD-785/786)
        </Button>
      </div>
    </section>
  );
}

/* ------------------------------------------------------------------ */
/* New + detail pages.                                                 */
/* ------------------------------------------------------------------ */

function CampaignNew({
  scope,
  viewer,
  onCreated,
  onCancel,
}: {
  scope: AudienceScope;
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
          · Context {scope.contextId || "none"} — audience previews need no save; content preview,
          test-send and proposals unlock after the first save.
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
          Pick an App context above before creating a campaign.
        </p>
      ) : (
        <>
          <div className="crm-field">
            <label className="text-label text-ink-300" htmlFor="cmp-id">
              Campaign ID (letters, digits, - _)
            </label>
            <input
              id="cmp-id"
              className="field"
              value={campaignId}
              onChange={(e) => setCampaignId(e.target.value)}
              maxLength={128}
              autoComplete="off"
              disabled={doc !== null}
            />
          </div>
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
  viewer,
  campaignId,
  onBack,
}: {
  scope: AudienceScope;
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
          <span className="num">
            · {campaignId} · {scope.contextId || "no context"}
            {doc !== null ? ` · r${doc.revision} · ${doc.contentDigest.slice(0, 18)}…` : ""}
          </span>
        </p>
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
            viewer={viewer}
            campaignId={campaignId}
            doc={doc}
            onDoc={setDoc}
          />
          <section aria-label="Frozen audience" className="card px-4 py-4 grid gap-3">
            <h4 className="text-cardtitle font-medium text-ink-100">Frozen audience</h4>
            <p className="text-label text-ink-400">
              Freezes are context-scoped rows addressed by operator-chosen IDs — the host stores
              no campaign-to-audience link, so this panel names the freeze explicitly (default{" "}
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
        </>
      )}
    </section>
  );
}

export type { AudienceScope };
