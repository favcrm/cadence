import { useEffect, useRef, useState } from "react";
import { ApiError } from "../../lib/api";
import Button from "../../ui/Button";
import Select from "../../ui/Select";
import type { Viewer } from "../projects/work";
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
          canWrite={viewer.operator && !viewer.readOnly}
          onClose={() => onSelect(null)}
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
  const canWrite = viewer.operator && !viewer.readOnly;
  const [segments, setSegments] = useState<SegmentDoc[]>([]);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);
  const [retry, setRetry] = useState(0);

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
      <div className="crm-toolbar">
        <p className="text-secondary text-ink-300">
          Saved rules over customer tags, source, consent and email domain.
        </p>
        <span className="flex-1" />
        {canWrite && (
          <Button variant="primary" size="sm" onClick={onNew}>
            New segment
          </Button>
        )}
      </div>
      {!viewer.operator && (
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
          Pick an App context above to list its segments.
        </p>
      )}
      {scope.contextId !== "" && viewer.operator && loading && (
        <p className="text-secondary text-ink-400" role="status">
          Reading segments…
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
      {scope.contextId !== "" && viewer.operator && error === null && !loading && segments.length === 0 && (
        <div className="card px-4 py-5 text-secondary text-ink-400" data-empty="segments" role="status">
          <p className="font-medium text-ink-200">No segments yet in this context</p>
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
                <th scope="col">Rules</th>
                <th scope="col">Rev</th>
                <th scope="col">
                  <span className="sr-only">Open</span>
                </th>
              </tr>
            </thead>
            <tbody>
              {segments.map((segment) => (
                <tr key={segment.id}>
                  <td className="text-ink-100">
                    {segment.name}
                    <span className="num text-micro text-ink-500"> · {segment.id}</span>
                  </td>
                  <td className="num text-ink-300">
                    {segment.predicates.length} rule{segment.predicates.length === 1 ? "" : "s"}
                  </td>
                  <td className="num text-ink-500">r{segment.revision}</td>
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
            <div className="crm-field">
              <label className="text-label text-ink-300" htmlFor={`seg-rule-field-${index}`}>
                Field {index + 1}
              </label>
              <Select
                id={`seg-rule-field-${index}`}
                value={rule.field}
                onChange={(value) => isSegmentField(value) && set(index, { field: value })}
                options={SEGMENT_FIELDS.map((entry) => ({ value: entry.value, label: entry.label }))}
                aria-label={`Rule ${index + 1} field`}
                disabled={disabled}
                full
              />
            </div>
            <div className="crm-field">
              <label className="text-label text-ink-300" htmlFor={`seg-rule-op-${index}`}>
                Operator {index + 1}
              </label>
              <Select
                id={`seg-rule-op-${index}`}
                value={rule.op}
                onChange={(value) => isSegmentOp(value) && set(index, { op: value })}
                options={SEGMENT_OPS.map((entry) => ({ value: entry.value, label: entry.label }))}
                aria-label={`Rule ${index + 1} operator`}
                disabled={disabled}
                full
              />
            </div>
          </div>
          <div className="crm-field mt-2">
            <label className="text-label text-ink-300" htmlFor={`seg-rule-value-${index}`}>
              Value {index + 1} — {ruleHint(rule.field)}
            </label>
            <input
              id={`seg-rule-value-${index}`}
              className="field"
              value={rule.value}
              onChange={(e) => set(index, { value: e.target.value })}
              maxLength={120}
              autoComplete="off"
              disabled={disabled}
              placeholder={rule.field === "consent_email" ? "granted" : rule.field === "email_domain" ? "example.com" : "vip"}
            />
          </div>
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
  const canWrite = viewer.operator && !viewer.readOnly;
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
        · Context {scope.contextId || "none"} — exact host preview counts render on the detail
        after Create.
      </p>
      {!canWrite ? (
        <p className="card px-4 py-3 mt-2 text-label text-ink-400" data-state="read-only">
          Read-only view. A verified operator creates saved segments.
        </p>
      ) : scope.contextId === "" ? (
        <p className="card px-4 py-3 mt-2 text-label text-ink-400">
          Pick an App context above before creating a segment.
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
          <div className="crm-field">
            <label className="text-label text-ink-300" htmlFor="seg-name">
              Segment name (required)
            </label>
            <input
              id="seg-name"
              className="field"
              value={name}
              onChange={(e) => setName(e.target.value)}
              maxLength={80}
              autoComplete="off"
              required
            />
          </div>
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
}: {
  scope: AudienceScope;
  segmentId: string;
  viewer: Viewer;
  canWrite: boolean;
  onClose: () => void;
}) {
  const [segment, setSegment] = useState<SegmentDoc | null>(null);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);
  const [editing, setEditing] = useState(false);
  const [preview, setPreview] = useState<AudiencePreview | null>(null);
  const [previewLoading, setPreviewLoading] = useState(true);
  const [previewError, setPreviewError] = useState<string | null>(null);
  const [previewToken, setPreviewToken] = useState(0);
  const headRef = useRef<HTMLHeadingElement | null>(null);
  const opener = useRef<Element | null>(null);

  useEffect(() => {
    opener.current = document.activeElement;
    headRef.current?.focus();
    return () => {
      if (opener.current instanceof HTMLElement) opener.current.focus();
    };
  }, []);
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") onClose();
    };
    addEventListener("keydown", onKey);
    return () => removeEventListener("keydown", onKey);
  }, [onClose]);

  const reloadToken = `${scope.installId}:${scope.contextId}:${segmentId}`;
  useEffect(() => {
    const controller = new AbortController();
    setLoading(true);
    setError(null);
    setEditing(false);
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

  return (
    <div
      className="crm-drawer"
      role="dialog"
      aria-modal="false"
      aria-label="Segment details"
      data-drawer="segment"
    >
      <div className="crm-drawer-head">
        <h3 ref={headRef} className="text-cardtitle font-medium text-ink-100" tabIndex={-1}>
          {loading ? "Segment details" : (segment?.name ?? "Segment details")}
        </h3>
        <Button size="sm" onClick={onClose} aria-label="Close segment details">
          Close
        </Button>
      </div>
      <p className="num text-micro text-ink-500">
        {segmentId} · {scope.contextId || "no context"}
      </p>
      {loading && (
        <p className="text-secondary text-ink-400 mt-2" role="status">
          Reading the segment…
        </p>
      )}
      {error !== null && !loading && (
        <p className="card px-4 py-3 mt-2 text-label text-fail border-fail/40" role="alert">
          {error}{" "}
          <button
            type="button"
            className="lnk"
            onClick={() => {
              setError(null);
              setLoading(true);
              audienceClient
                .segmentShow(scope, segmentId)
                .then((value) => setSegment(parseSegment(value)))
                .catch((e: unknown) => setError(friendlyAudienceError(e)))
                .finally(() => setLoading(false));
            }}
          >
            Retry
          </button>
        </p>
      )}
      {!loading && error === null && segment !== null && (
        <>
          <dl className="crm-detail mt-2">
            <div>
              <dt>Revision</dt>
              <dd className="num">
                r{segment.revision} · <span title="Server rule digest">{segment.digest.slice(0, 18)}…</span>
              </dd>
            </div>
          </dl>
          <section aria-label="Segment rules" className="mt-3">
            <h4 className="text-label font-medium text-ink-200">Rules ({segment.predicates.length})</h4>
            <ol className="crm-history">
              {segment.predicates.map((rule, index) => (
                <li key={index} className="num text-label text-ink-300">
                  {rule.field} {rule.op} {rule.value}
                </li>
              ))}
            </ol>
          </section>
          <section aria-label="Current matches" className="mt-3">
            <h4 className="text-label font-medium text-ink-200">Current matches</h4>
            <div className="mt-1">
              <PreviewPanel
                preview={preview}
                loading={previewLoading}
                error={previewError}
                onRetry={() => setPreviewToken((count) => count + 1)}
                label="Current matches"
              />
            </div>
          </section>
          {canWrite ? (
            editing ? (
              <SegmentEdit
                scope={scope}
                segment={segment}
                onSaved={(next) => {
                  setSegment(next);
                  setEditing(false);
                }}
                onCancel={() => setEditing(false)}
              />
            ) : (
              <p className="mt-3">
                <Button size="sm" onClick={() => setEditing(true)}>
                  Edit rules
                </Button>
              </p>
            )
          ) : (
            <p className="text-label text-ink-400 mt-3" data-state="read-only">
              Read-only view. {viewer.operator ? "Edits are disabled on this board." : "Sign in as the operator to edit."}
            </p>
          )}
        </>
      )}
    </div>
  );
}

function SegmentEdit({
  scope,
  segment,
  onSaved,
  onCancel,
}: {
  scope: AudienceScope;
  segment: SegmentDoc;
  onSaved: (segment: SegmentDoc) => void;
  onCancel: () => void;
}) {
  const [name, setName] = useState(segment.name);
  const [rules, setRules] = useState<RuleDraft[]>(() =>
    segment.predicates.map((rule) => ({ field: rule.field, op: rule.op, value: rule.value })),
  );
  const [pending, setPending] = useState(false);
  const [formError, setFormError] = useState<string | null>(null);
  return (
    <form
      className="card px-4 py-4 mt-3 grid gap-3"
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
            segmentId: segment.id,
            name,
            predicates: rules.map((rule) => ({ field: rule.field, op: rule.op, value: rule.value })),
            expectedRevision: segment.revision,
          })
          .then((value) => onSaved(parseSegment(value)))
          .catch((err: unknown) => setFormError(friendlyAudienceError(err)))
          .finally(() => setPending(false));
      }}
    >
      <h4 className="text-label font-medium text-ink-200">
        Edit — expected revision r{segment.revision}
      </h4>
      <div className="crm-field">
        <label className="text-label text-ink-300" htmlFor="seg-edit-name">
          Segment name
        </label>
        <input
          id="seg-edit-name"
          className="field"
          value={name}
          onChange={(e) => setName(e.target.value)}
          maxLength={80}
          autoComplete="off"
          disabled={pending}
          required
        />
      </div>
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
          {formError} A stale revision means another operator saved first — close and reopen to
          review their rules before retrying.
        </p>
      )}
      <div className="crm-toolbar">
        <Button type="submit" variant="primary" loading={pending} disabled={pending}>
          {`Save as r${segment.revision + 1}`}
        </Button>
        <button type="button" className="lnk text-label" onClick={onCancel}>
          Discard edit
        </button>
      </div>
    </form>
  );
}
