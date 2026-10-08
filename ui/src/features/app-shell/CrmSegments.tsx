import { useEffect, useRef, useState } from "react";
import { ApiError } from "../../lib/api";
import { navigate, useHref } from "../../lib/useLocation";
import Button from "../../ui/Button";
import Select from "../../ui/Select";
import type { Viewer } from "../projects/work";
import Field from "./shared/Field";
import { ErrorNotice } from "./shared/States";
import DrawerShell, { type DrawerTab } from "./shared/DrawerShell";
import {
  audienceClient,
  type AudienceScope,
  type SegmentPredicate,
} from "./audienceClient";
import {
  checkPredicate,
  checkSegmentName,
  friendlyAudienceError,
  isSegmentField,
  isSegmentOp,
  newAudienceId,
  SEGMENT_FIELDS,
  SEGMENT_OPS,
} from "./segmentGrammar";

/** Human-readable predicate: the same labels the rule form offers.
 *  Unknown stored values fall back to the raw tokens, never hide. */
export function describeRule(rule: SegmentPredicate): string {
  const field = SEGMENT_FIELDS.find((f) => f.value === rule.field)?.label ?? rule.field;
  const op = SEGMENT_OPS.find((o) => o.value === rule.op)?.label ?? rule.op;
  return `${field} ${op} ${rule.value}`;
}

/** Rule as a sentence of tokens: "Customers where [tag] [is] [vip]". */
function RuleSentence({ rules }: { rules: SegmentPredicate[] }) {
  return (
    <p className="crm-rule-sentence" data-testid="rule-sentence">
      <span>Customers where</span>
      {rules.map((rule, i) => (
        <span key={i} className="crm-rule-sentence">
          {i > 0 && <span>and</span>}
          <span className="chip">{SEGMENT_FIELDS.find((f) => f.value === rule.field)?.label ?? rule.field}</span>
          <span className="chip">{SEGMENT_OPS.find((o) => o.value === rule.op)?.label ?? rule.op}</span>
          <span className="chip">{rule.value}</span>
        </span>
      ))}
    </p>
  );
}

export interface FunnelStep {
  label: string;
  count: number;
}

/** Eligibility funnel built only from the host's membership-preview
 *  counts, plus the one plain-language reason that blocks most people. */
export function eligibilityFunnel(p: AudiencePreview): { steps: FunnelStep[]; reason: string | null } {
  const valid = Math.max(p.finalCount, p.baseCount - p.excluded.invalid);
  const consent = Math.max(p.finalCount, valid - p.excluded.noConsent - p.excluded.unsubscribed);
  const steps = [
    { label: "Match the rule", count: p.baseCount },
    { label: "Valid email", count: valid },
    { label: "Email consent", count: consent },
    { label: "Can be emailed", count: p.finalCount },
  ];
  const causes: [number, string][] = [
    [p.excluded.noConsent, "have not agreed to receive email"],
    [p.excluded.unsubscribed, "have unsubscribed"],
    [p.excluded.invalid, "have no valid email address"],
    [p.excluded.suppressed, "are on the suppression list"],
    [p.exclusionCount, "are on the saved exclusion list"],
  ];
  const top = causes.filter(([n]) => n > 0).sort((a, b) => b[0] - a[0])[0];
  let reason: string | null = null;
  if (p.baseCount === 0) reason = "No customers match this rule yet.";
  else if (p.finalCount === 0 && top) reason = `Nobody can be emailed: ${top[0]} ${top[1]}.`;
  else if (p.finalCount < p.baseCount && top) reason = `${top[0]} ${top[0] === 1 ? "customer" : "customers"} ${top[1]}.`;
  return { steps, reason };
}

/**
 * Saved-rule segment screens inside the trusted CRM shell (CAD-784
 * over the CAD-780 audience engine). List, separate New page and a
 * right detail drawer with the host's exact current-match preview —
 * no sample rows anywhere. The outlet remounts on every
 * install/context switch (the shell keys it on scope), so no draft
 * survives the boundary.
 *
 * A New segment cannot preview before it exists: the host preview
 * action addresses a saved rule, so Create lands atomically on the
 * detail drawer where the exact counts, suppression breakdown and
 * bounded sample render.
 *
 * The shell's own header row is the single Apps → App breadcrumb and
 * title; the section renders its own real heading instead of a
 * second crumb (CAD-863 release correction).
 */

export interface SegmentDoc {
  id: string;
  revision: number;
  name: string;
  predicates: SegmentPredicate[];
  digest: string;
}

function parseSegment(value: unknown): SegmentDoc {
  const doc = (value as { segment?: unknown } | null)?.segment;
  if (!doc || typeof doc !== "object") {
    throw new ApiError("The server returned an invalid segment receipt", 502);
  }
  const row = doc as Record<string, unknown>;
  if (
    typeof row.id !== "string" ||
    typeof row.revision !== "number" ||
    typeof row.name !== "string" ||
    typeof row.digest !== "string" ||
    !Array.isArray(row.predicates)
  ) {
    throw new ApiError("The server returned an invalid segment receipt", 502);
  }
  return {
    id: row.id,
    revision: row.revision,
    name: row.name,
    predicates: row.predicates as SegmentPredicate[],
    digest: row.digest,
  };
}

function parseSegmentList(value: unknown): SegmentDoc[] {
  const rows = (value as { segments?: unknown } | null)?.segments;
  if (!Array.isArray(rows)) {
    throw new ApiError("The server returned an invalid segment receipt", 502);
  }
  return rows.map((row) => parseSegment({ segment: row }));
}

export interface AudiencePreview {
  baseCount: number;
  exclusionCount: number;
  excluded: { invalid: number; noConsent: number; unsubscribed: number; suppressed: number };
  finalCount: number;
  sample: { id: string; displayName: string }[];
  digest: string;
}

export function parsePreview(value: unknown): AudiencePreview {
  const root = (value as Record<string, unknown> | null) ?? {};
  const excluded = (root.final_excluded as Record<string, unknown> | undefined) ?? {};
  const sample = Array.isArray(root.sample) ? root.sample : [];
  const num = (slot: unknown): number => (typeof slot === "number" ? slot : -1);
  if (
    num(root.base_count) < 0 ||
    num(root.final_count) < 0 ||
    num(excluded.invalid_email) < 0 ||
    num(excluded.no_consent) < 0 ||
    num(excluded.unsubscribed) < 0 ||
    num(excluded.suppressed) < 0 ||
    typeof root.digest !== "string"
  ) {
    throw new ApiError("The server returned an invalid audience receipt", 502);
  }
  return {
    baseCount: root.base_count as number,
    exclusionCount: typeof root.exclusion_count === "number" ? root.exclusion_count : 0,
    excluded: {
      invalid: excluded.invalid_email as number,
      noConsent: excluded.no_consent as number,
      unsubscribed: excluded.unsubscribed as number,
      suppressed: excluded.suppressed as number,
    },
    finalCount: root.final_count as number,
    sample: sample.map((entry) => {
      const row = (entry as Record<string, unknown> | null) ?? {};
      return {
        id: typeof row.id === "string" ? row.id : "",
        displayName: typeof row.display_name === "string" ? row.display_name : "",
      };
    }),
    digest: root.digest as string,
  };
}

export default function CrmSegments({
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
    <div className="crm-list" data-section="segments">
      {view === "list" && (
        <SegmentList
          scope={scope}
          viewer={viewer}
          onSelect={onSelect}
          onNew={() => onView("new")}
        />
      )}
      {view === "new" && (
        <SegmentNew
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
      {recordId !== null && (
        <SegmentDrawer
          scope={scope}
          segmentId={recordId}
          viewer={viewer}
          canWrite={viewer.operator === true && !viewer.readOnly}
          onClose={() => onSelect(null)}
          onOpen={onSelect}
        />
      )}
    </div>
  );
}

function SegmentList({
  scope,
  viewer,
  onSelect,
  onNew,
}: {
  scope: AudienceScope;
  viewer: Viewer;
  onSelect: (segmentId: string) => void;
  onNew: () => void;
}) {
  const canWrite = viewer.operator === true && !viewer.readOnly;
  const [segments, setSegments] = useState<SegmentDoc[]>([]);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);
  const [retry, setRetry] = useState(0);
  const [counts, setCounts] = useState<Record<string, AudiencePreview | null>>({});

  // Per-row exact host counts (bounded); a failed row shows an em dash.
  useEffect(() => {
    const controller = new AbortController();
    for (const segment of segments.slice(0, 25)) {
      audienceClient
        .preview(scope, { mode: "segment", segmentId: segment.id })
        .then((value) => {
          if (!controller.signal.aborted) setCounts((c) => ({ ...c, [segment.id]: parsePreview(value) }));
        })
        .catch(() => {
          if (!controller.signal.aborted) setCounts((c) => ({ ...c, [segment.id]: null }));
        });
    }
    return () => controller.abort();
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [segments]);

  const reloadToken = `${scope.installId}:${scope.contextId}:${retry}`;
  useEffect(() => {
    const controller = new AbortController();
    setLoading(true);
    setError(null);
    audienceClient
      .segmentList(scope)
      .then((value) => {
        if (!controller.signal.aborted) setSegments(parseSegmentList(value));
      })
      .catch((e: unknown) => {
        if (!controller.signal.aborted) setError(friendlyAudienceError(e));
      })
      .finally(() => {
        if (!controller.signal.aborted) setLoading(false);
      });
    return () => controller.abort();
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [reloadToken]);

  return (
    <section aria-label="Segments list" className="crm-list">
      <h3 className="text-cardtitle font-medium text-ink-100" data-outlet-heading>
        Segments
      </h3>
      <div className="crm-toolbar mb-4">
        <p className="crm-toolbar-lede text-secondary text-ink-300">
          Saved rules over customer tags, source, consent and email domain.
        </p>
        {canWrite && (
          <Button variant="primary" size="sm" onClick={onNew}>
            New segment
          </Button>
        )}
      </div>
      {viewer.operator === null && (
        viewer.access === "unavailable" ? (
          // CAD-1193: a completed unanswered check is unavailable, not
          // still checking — the retry re-runs the real access probe.
          <p className="card px-4 py-3 text-label text-ink-400" data-state="access-unavailable">
            Access could not be confirmed — the board's access check did
            not answer, so saved segments stay closed.{" "}
            {viewer.onRetryAccess && (
              <button type="button" className="lnk" onClick={viewer.onRetryAccess}>
                Retry access check
              </button>
            )}
          </p>
        ) : (
          <p className="card px-4 py-3 text-label text-ink-400" role="status">
            Checking whether this session may inspect saved segments…
          </p>
        )
      )}
      {viewer.operator === false && (
        <p className="card px-4 py-3 text-label text-ink-400">
          Sign in as the operator to inspect saved segments.
        </p>
      )}
      {viewer.operator && viewer.readOnly && (
        <p className="card px-4 py-3 text-label text-ink-400" data-state="read-only">
          Read-only view. Segment creation and edits are unavailable.
        </p>
      )}
      {scope.contextId === "" && viewer.operator && (
        <p className="card px-4 py-3 text-label text-ink-400">
          Administrator CRM setup is required before segments open.
        </p>
      )}
      {scope.contextId !== "" && viewer.operator && loading && (
        <p className="text-secondary text-ink-400" role="status">
          Reading segments…
        </p>
      )}
      {scope.contextId !== "" && viewer.operator && error !== null && !loading && (
        <ErrorNotice onRetry={() => setRetry((count) => count + 1)}>{error}</ErrorNotice>
      )}
      {scope.contextId !== "" && viewer.operator && error === null && !loading && segments.length === 0 && (
        <div className="card px-4 py-5 text-secondary text-ink-400" data-empty="segments" role="status">
          <p className="font-medium text-ink-200">No segments yet</p>
          <p className="mt-1">
            Create the first saved rule with New segment. Only real server rows appear here.
          </p>
        </div>
      )}
      {scope.contextId !== "" && viewer.operator && error === null && segments.length > 0 && (
        <div
          className="crm-table-wrap"
          tabIndex={0}
          role="region"
          aria-label="Segments table — scroll horizontally to reach every column"
        >
          <table className="crm-table">
            <thead>
              <tr>
                <th scope="col">Name</th>
                <th scope="col">Rule</th>
                <th scope="col">Matches</th>
                <th scope="col">Can be emailed</th>
                <th scope="col">
                  <span className="sr-only">Open</span>
                </th>
              </tr>
            </thead>
            <tbody>
              {segments.map((segment) => (
                <tr
                  key={segment.id}
                  data-record-id={segment.id}
                  className="crm-row-open"
                  onClick={(e) => {
                    // A click on a control inside the row keeps its own meaning.
                    if ((e.target as HTMLElement).closest("button, input, a")) return;
                    onSelect(segment.id);
                  }}
                >
                  <td className="text-ink-100">
                    {segment.name}
                    <span className="num text-micro text-ink-500"> · {segment.id}</span>
                  </td>
                  <td className="text-ink-300">
                    <span className="chip">{segment.predicates[0] ? describeRule(segment.predicates[0]) : "—"}</span>
                    {segment.predicates.length > 1 && (
                      <span className="num text-micro text-ink-500"> +{segment.predicates.length - 1} more</span>
                    )}
                  </td>
                  <td className="num text-ink-300">{counts[segment.id]?.baseCount ?? "—"}</td>
                  <td>
                    {counts[segment.id] ? (
                      <span className="chip" data-tone={counts[segment.id]!.finalCount === 0 ? "warn" : "ok"}>
                        {counts[segment.id]!.finalCount}
                      </span>
                    ) : (
                      "—"
                    )}
                  </td>
                  <td>
                    <button type="button" className="lnk" onClick={() => onSelect(segment.id)}>
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

export interface RuleDraft {
  field: string;
  op: string;
  value: string;
}

export function ruleHint(field: string): string {
  return SEGMENT_FIELDS.find((entry) => entry.value === field)?.hint ?? "";
}

function RuleRows({
  rules,
  disabled,
  onChange,
}: {
  rules: RuleDraft[];
  disabled: boolean;
  onChange: (rules: RuleDraft[]) => void;
}) {
  const set = (index: number, patch: Partial<RuleDraft>) => {
    onChange(rules.map((rule, at) => (at === index ? { ...rule, ...patch } : rule)));
  };
  return (
    <ol className="crm-history" aria-label="Segment rules">
      {rules.map((rule, index) => (
        <li key={index} className="card px-3 py-3">
          <div className="crm-field-row">
            <Field label={`Field ${index + 1}`} id={`seg-rule-field-${index}`} className="crm-field">
              {(c) => (
                <Select
                  id={c.id}
                  value={rule.field}
                  onChange={(value) => isSegmentField(value) && set(index, { field: value })}
                  options={SEGMENT_FIELDS.map((entry) => ({ value: entry.value, label: entry.label }))}
                  aria-label={`Rule ${index + 1} field`}
                  disabled={disabled}
                  full
                />
              )}
            </Field>
            <Field label={`Operator ${index + 1}`} id={`seg-rule-op-${index}`} className="crm-field">
              {(c) => (
                <Select
                  id={c.id}
                  value={rule.op}
                  onChange={(value) => isSegmentOp(value) && set(index, { op: value })}
                  options={SEGMENT_OPS.map((entry) => ({ value: entry.value, label: entry.label }))}
                  aria-label={`Rule ${index + 1} operator`}
                  disabled={disabled}
                  full
                />
              )}
            </Field>
          </div>
          <Field
            label={`Value ${index + 1}`}
            id={`seg-rule-value-${index}`}
            hint={ruleHint(rule.field)}
            disabled={disabled}
            className="crm-field mt-2"
          >
            {(c) => (
              <input
                {...c}
                className="field"
                value={rule.value}
                onChange={(e) => set(index, { value: e.target.value })}
                maxLength={120}
                autoComplete="off"
                placeholder={rule.field === "consent_email" ? "granted" : rule.field === "email_domain" ? "example.com" : "vip"}
              />
            )}
          </Field>
          {rules.length > 1 && !disabled && (
            <p className="mt-2">
              <button
                type="button"
                className="lnk text-label"
                onClick={() => onChange(rules.filter((_, at) => at !== index))}
              >
                Remove rule {index + 1}
              </button>
            </p>
          )}
        </li>
      ))}
    </ol>
  );
}

function SegmentNew({
  scope,
  viewer,
  onCreated,
  onCancel,
}: {
  scope: AudienceScope;
  viewer: Viewer;
  onCreated: (segmentId: string) => void;
  onCancel: () => void;
}) {
  const canWrite = viewer.operator === true && !viewer.readOnly;
  const headRef = useRef<HTMLHeadingElement | null>(null);
  const [name, setName] = useState("");
  const [rules, setRules] = useState<RuleDraft[]>([{ field: "tag", op: "eq", value: "" }]);
  const [pending, setPending] = useState(false);
  const [formError, setFormError] = useState<string | null>(null);
  useEffect(() => {
    headRef.current?.focus();
  }, []);
  return (
    <section aria-label="New segment">
      <h3 ref={headRef} className="text-cardtitle font-medium text-ink-100" tabIndex={-1} data-outlet-heading>
        New segment
      </h3>
      <p className="text-label text-ink-400 mt-1">
        <button type="button" className="lnk" onClick={onCancel}>
          ← Segments
        </button>{" "}
        — exact recipient counts render on the detail after Create.
      </p>
      {!canWrite ? (
        <p className="card px-4 py-3 mt-2 text-label text-ink-400" data-state="read-only">
          Read-only view. A verified operator creates saved segments.
        </p>
      ) : scope.contextId === "" ? (
        <p className="card px-4 py-3 mt-2 text-label text-ink-400">
          Administrator CRM setup is required before creating a segment.
        </p>
      ) : (
        <form
          className="card px-4 py-4 mt-2 grid gap-3"
          onSubmit={(e) => {
            e.preventDefault();
            setFormError(null);
            try {
              checkSegmentName(name);
              if (rules.length === 0 || rules.length > 8) {
                throw new ApiError("a segment needs 1 to 8 rules", 400);
              }
              for (const rule of rules) checkPredicate(rule.field, rule.op, rule.value);
            } catch (err: unknown) {
              setFormError(friendlyAudienceError(err));
              return;
            }
            setPending(true);
            void audienceClient
              .segmentSave(scope, {
                segmentId: newAudienceId("seg"),
                name,
                predicates: rules.map((rule) => ({ field: rule.field, op: rule.op, value: rule.value })),
              })
              .then((value) => onCreated(parseSegment(value).id))
              .catch((err: unknown) => setFormError(friendlyAudienceError(err)))
              .finally(() => setPending(false));
          }}
        >
          <Field label="Segment name" id="seg-name" required className="crm-field">
            {(c) => (
              <input
                {...c}
                className="field"
                value={name}
                onChange={(e) => setName(e.target.value)}
                maxLength={80}
                autoComplete="off"
              />
            )}
          </Field>
          <RuleRows rules={rules} disabled={pending} onChange={setRules} />
          {rules.length < 8 && (
            <p>
              <button
                type="button"
                className="lnk text-label"
                disabled={pending}
                onClick={() => setRules((prev) => [...prev, { field: "tag", op: "eq", value: "" }])}
              >
                + Add rule ({rules.length}/8)
              </button>
            </p>
          )}
          {formError && (
            <p className="text-label text-fail" role="alert">
              {formError}
            </p>
          )}
          <div>
            <Button type="submit" variant="primary" loading={pending} disabled={pending}>
              Create segment
            </Button>
          </div>
        </form>
      )}
    </section>
  );
}

export function PreviewPanel({
  preview,
  loading,
  error,
  onRetry,
  label,
}: {
  preview: AudiencePreview | null;
  loading: boolean;
  error: string | null;
  onRetry: () => void;
  label: string;
}) {
  if (loading) {
    return (
      <p className="text-secondary text-ink-400" role="status">
        Reading host audience counts…
      </p>
    );
  }
  if (error !== null) {
    return (
      <p className="card px-4 py-3 text-label text-fail border-fail/40" role="alert">
        {error}{" "}
        <button type="button" className="lnk" onClick={onRetry}>
          Retry
        </button>
      </p>
    );
  }
  if (preview === null) return null;
  return (
    <div aria-label={label}>
      <dl className="crm-detail">
        <div>
          <dt>Base matches</dt>
          <dd className="num">{preview.baseCount}</dd>
        </div>
        <div>
          <dt>Saved exclusions</dt>
          <dd className="num">{preview.exclusionCount}</dd>
        </div>
        <div>
          <dt>Invalid address</dt>
          <dd className="num">{preview.excluded.invalid}</dd>
        </div>
        <div>
          <dt>No consent</dt>
          <dd className="num">{preview.excluded.noConsent}</dd>
        </div>
        <div>
          <dt>Unsubscribed</dt>
          <dd className="num">{preview.excluded.unsubscribed}</dd>
        </div>
        <div>
          <dt>Suppressed</dt>
          <dd className="num">{preview.excluded.suppressed}</dd>
        </div>
        <div>
          <dt>Final recipients</dt>
          <dd className="num">{preview.finalCount}</dd>
        </div>
        <div>
          <dt>Preview digest</dt>
          <dd className="num" title="Host audience digest">
            {preview.digest.slice(0, 18)}…
          </dd>
        </div>
      </dl>
      {preview.sample.length > 0 && (
        <section aria-label="Audience sample" className="mt-3">
          <h4 className="text-label font-medium text-ink-200">
            Sample ({preview.sample.length} of {preview.finalCount})
          </h4>
          <ol className="crm-history">
            {preview.sample.map((entry) => (
              <li key={entry.id} className="num text-label text-ink-300">
                {entry.displayName || "—"} · {entry.id}
              </li>
            ))}
          </ol>
        </section>
      )}
    </div>
  );
}

function SegmentDrawer({
  scope,
  segmentId,
  viewer,
  canWrite,
  onClose,
  onOpen,
}: {
  scope: AudienceScope;
  segmentId: string;
  viewer: Viewer;
  canWrite: boolean;
  onClose: () => void;
  onOpen: (segmentId: string) => void;
}) {
  const [segment, setSegment] = useState<SegmentDoc | null>(null);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);
  const [editing, setEditing] = useState(false);
  const [editPending, setEditPending] = useState(false);
  const [preview, setPreview] = useState<AudiencePreview | null>(null);
  const [previewLoading, setPreviewLoading] = useState(true);
  const [previewError, setPreviewError] = useState<string | null>(null);
  const [previewToken, setPreviewToken] = useState(0);
  const [tab, setTab] = useState("overview");
  const href = useHref();
  const reloadToken = `${scope.installId}:${scope.contextId}:${segmentId}`;
  useEffect(() => {
    const controller = new AbortController();
    setLoading(true);
    setError(null);
    setEditing(false);
    setTab("overview");
    audienceClient
      .segmentShow(scope, segmentId)
      .then((value) => {
        if (!controller.signal.aborted) setSegment(parseSegment(value));
      })
      .catch((e: unknown) => {
        if (!controller.signal.aborted) setError(friendlyAudienceError(e));
      })
      .finally(() => {
        if (!controller.signal.aborted) setLoading(false);
      });
    return () => controller.abort();
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [reloadToken]);

  // Exact host preview counts for the saved rule: the same preview
  // action campaigns freeze from, never a client estimate.
  useEffect(() => {
    if (segment === null) return;
    const controller = new AbortController();
    setPreviewLoading(true);
    setPreviewError(null);
    audienceClient
      .preview(scope, { mode: "segment", segmentId: segment.id })
      .then((value) => {
        if (!controller.signal.aborted) setPreview(parsePreview(value));
      })
      .catch((e: unknown) => {
        if (!controller.signal.aborted) setPreviewError(friendlyAudienceError(e));
      })
      .finally(() => {
        if (!controller.signal.aborted) setPreviewLoading(false);
      });
    return () => controller.abort();
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [segment?.id, segment?.revision, previewToken]);

  const reload = () => {
    setError(null);
    setLoading(true);
    audienceClient
      .segmentShow(scope, segmentId)
      .then((value) => setSegment(parseSegment(value)))
      .catch((e: unknown) => setError(friendlyAudienceError(e)))
      .finally(() => setLoading(false));
  };
  const ready = !loading && error === null && segment !== null;

  const members = preview?.sample ?? [];
  const funnel = preview ? eligibilityFunnel(preview) : null;
  const memberList = (rows: { id: string; displayName: string }[]) => (
    <ul className="crm-history" aria-label="Segment members">
      {rows.map((m) => (
        <li key={m.id} className="text-label text-ink-300">
          {m.displayName || "—"} <span className="chip" data-tone="ok">Can email</span>
        </li>
      ))}
    </ul>
  );
  const tabs: DrawerTab[] =
    ready && segment !== null
      ? [
          {
            id: "overview",
            label: "Overview",
            panel: (
              <>
                <section aria-label="Segment rule">
                  <RuleSentence rules={segment.predicates} />
                </section>
                <section aria-label="Who can be emailed" className="mt-3">
                  <h4 className="text-label font-medium text-ink-200">Who can be emailed</h4>
                  {previewLoading && (
                    <p className="text-secondary text-ink-400" role="status">Reading host audience counts…</p>
                  )}
                  {previewError !== null && (
                    <p className="card px-4 py-3 text-label text-fail border-fail/40" role="alert">
                      {previewError}{" "}
                      <button type="button" className="lnk" onClick={() => setPreviewToken((n) => n + 1)}>Retry</button>
                    </p>
                  )}
                  {funnel && !previewLoading && previewError === null && (
                    <>
                      <ol className="crm-funnel" data-testid="funnel">
                        {funnel.steps.map((step) => (
                          <li key={step.label}>
                            <span>{step.label}</span>
                            <span className="crm-funnel-bar">
                              <span style={{ width: `${funnel.steps[0].count === 0 ? 0 : Math.round((step.count / funnel.steps[0].count) * 100)}%` }} />
                            </span>
                            <span className="num">{step.count}</span>
                          </li>
                        ))}
                      </ol>
                      {funnel.reason !== null && (
                        <p className="text-label text-ink-300 mt-1" data-testid="funnel-reason">{funnel.reason}</p>
                      )}
                    </>
                  )}
                </section>
                <section aria-label="Members preview" className="mt-3">
                  <h4 className="text-label font-medium text-ink-200">Members</h4>
                  {members.length === 0 && !previewLoading && (
                    <p className="text-secondary text-ink-400">No one can be emailed from this segment yet.</p>
                  )}
                  {memberList(members.slice(0, 10))}
                  {members.length > 0 && (
                    <button type="button" className="lnk text-label" onClick={() => setTab("members")}>View all</button>
                  )}
                </section>
              </>
            ),
          },
          {
            id: "members",
            label: "Members",
            panel: (
              <section aria-label="All members">
                <p className="text-label text-ink-400">
                  Showing {members.length} of {preview?.finalCount ?? 0} customers who can be emailed.
                </p>
                {memberList(members)}
              </section>
            ),
          },
          {
            id: "details",
            label: "Details",
            panel: (
              <section aria-label="Record diagnostics">
                <p className="num text-micro text-ink-500">
                  Segment <span className="num">{segmentId}</span> · scope{" "}
                  <span className="num">{scope.contextId || "none"}</span> · revision r{segment.revision} · digest{" "}
                  {segment.digest.slice(0, 18)}…
                </p>
                <div className="mt-3">
                  <PreviewPanel
                    preview={preview}
                    loading={previewLoading}
                    error={previewError}
                    onRetry={() => setPreviewToken((count) => count + 1)}
                    label="Current matches"
                  />
                </div>
              </section>
            ),
          },
        ]
      : [];

  const duplicate = () => {
    if (segment === null) return;
    void audienceClient
      .segmentSave(scope, {
        segmentId: newAudienceId("seg"),
        name: `${segment.name} (copy)`.slice(0, 80),
        predicates: segment.predicates.map((r) => ({ field: r.field, op: r.op, value: r.value })),
      })
      .then((value) => onOpen(parseSegment(value).id))
      .catch((err: unknown) => setError(friendlyAudienceError(err)));
  };
  const useInCampaign = () => {
    const [path, search] = href.split("?");
    const q = new URLSearchParams(search ?? "");
    q.set("crm", "campaigns");
    q.set("appview", "new");
    q.set("segment", segmentId);
    q.delete("record");
    navigate(`${path}?${q.toString()}`);
  };

  return (
    <DrawerShell
      kind="segment"
      label="Segment details"
      title={loading ? "Segment details" : (segment?.name ?? "Segment details")}
      avatar="◎"
      subtitle={segment ? `${segment.predicates.length} rule${segment.predicates.length === 1 ? "" : "s"}` : undefined}
      pills={
        ready && preview ? (
          <>
            <span className="chip">{preview.baseCount} match</span>
            <span className="chip" data-tone={preview.finalCount === 0 ? "warn" : "ok"}>
              {preview.finalCount} can be emailed
            </span>
          </>
        ) : undefined
      }
      tabs={tabs}
      tab={tab}
      onTab={setTab}
      secondary={
        ready ? (
          <>
            <Button size="sm" onClick={duplicate} disabled={!canWrite} title={canWrite ? undefined : "Operators only"}>
              Duplicate
            </Button>
            {canWrite ? (
              <Button size="sm" onClick={() => setEditing(true)}>
                Edit rules
              </Button>
            ) : null}
          </>
        ) : undefined
      }
      state={
        loading ? (
          <p className="text-secondary text-ink-400" role="status">
            Reading the segment…
          </p>
        ) : error !== null ? (
          <p className="card px-4 py-3 text-label text-fail border-fail/40" role="alert">
            {error}{" "}
            <button type="button" className="lnk" onClick={reload}>
              Retry
            </button>
          </p>
        ) : undefined
      }
      edit={
        editing && canWrite && ready && segment !== null
          ? {
              formId: "crm-segment-edit",
              title: "Edit rules — saving is refused if the segment changed since you opened it",
              pending: editPending,
              onCancel: () => setEditing(false),
              body: (
                <SegmentEdit
                  formId="crm-segment-edit"
                  scope={scope}
                  segment={segment}
                  onPending={setEditPending}
                  onSaved={(next) => {
                    setSegment(next);
                    setEditing(false);
                  }}
                />
              ),
            }
          : null
      }
      primary={
        ready && canWrite ? (
          <Button size="sm" variant="primary" onClick={useInCampaign}>
            Use in campaign
          </Button>
        ) : undefined
      }
      note={
        ready && !canWrite
          ? `Read-only view. ${viewer.operator ? "Edits are disabled on this board." : "Sign in as the operator to edit."}`
          : undefined
      }
      onClose={onClose}
    />
  );
}

function SegmentEdit({
  formId,
  scope,
  segment,
  onPending,
  onSaved,
}: {
  formId: string;
  scope: AudienceScope;
  segment: SegmentDoc;
  onPending: (pending: boolean) => void;
  onSaved: (segment: SegmentDoc) => void;
}) {
  const [name, setName] = useState(segment.name);
  const [rules, setRules] = useState<RuleDraft[]>(() =>
    segment.predicates.map((rule) => ({ field: rule.field, op: rule.op, value: rule.value })),
  );
  const [pending, setPending] = useState(false);
  const [formError, setFormError] = useState<string | null>(null);
  return (
    <form
      id={formId}
      className="grid gap-3"
      onSubmit={(e) => {
        e.preventDefault();
        setFormError(null);
        try {
          checkSegmentName(name);
          if (rules.length === 0 || rules.length > 8) {
            throw new ApiError("a segment needs 1 to 8 rules", 400);
          }
          for (const rule of rules) checkPredicate(rule.field, rule.op, rule.value);
        } catch (err: unknown) {
          setFormError(friendlyAudienceError(err));
          return;
        }
        setPending(true);
        onPending(true);
        void audienceClient
          .segmentSave(scope, {
            segmentId: segment.id,
            name,
            predicates: rules.map((rule) => ({ field: rule.field, op: rule.op, value: rule.value })),
            expectedRevision: segment.revision,
          })
          .then((value) => onSaved(parseSegment(value)))
          .catch((err: unknown) => setFormError(friendlyAudienceError(err)))
          .finally(() => {
            setPending(false);
            onPending(false);
          });
      }}
    >
      <Field label="Segment name" id="seg-edit-name" required disabled={pending} className="crm-field">
        {(c) => (
          <input
            {...c}
            className="field"
            value={name}
            onChange={(e) => setName(e.target.value)}
            maxLength={80}
            autoComplete="off"
          />
        )}
      </Field>
      <RuleRows rules={rules} disabled={pending} onChange={setRules} />
      {rules.length < 8 && (
        <p>
          <button
            type="button"
            className="lnk text-label"
            disabled={pending}
            onClick={() => setRules((prev) => [...prev, { field: "tag", op: "eq", value: "" }])}
          >
            + Add rule ({rules.length}/8)
          </button>
        </p>
      )}
      {formError && (
        <p className="text-label text-fail" role="alert">
          {formError} If another operator saved first, close and reopen to review their rules
          before retrying.
        </p>
      )}
    </form>
  );
}
