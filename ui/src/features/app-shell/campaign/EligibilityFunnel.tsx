import type { AudiencePreview } from "../CrmSegments";

/**
 * Eligibility funnel (CAD-1055) over the host's existing audience
 * counts — no new wire. Each row is a count the host reported; the
 * final row is the host's own final count, never a figure derived
 * here. When nobody can be emailed, one plain-language line names the
 * biggest reason.
 */
export function blockingReason(preview: AudiencePreview): string | null {
  if (preview.finalCount > 0) return null;
  if (preview.baseCount === 0) return "No customers match this audience. Pick another segment.";
  const reasons: [number, string][] = [
    [preview.excluded.noConsent, "lack email consent. Record consent on those customers or pick another segment."],
    [preview.excluded.invalid, "have no valid email address."],
    [preview.excluded.unsubscribed, "have unsubscribed."],
    [preview.excluded.suppressed, "are suppressed."],
    [preview.exclusionCount, "are on the exclusion list."],
  ];
  const [count, why] = reasons.reduce((best, row) => (row[0] > best[0] ? row : best));
  if (count <= 0) return "Nobody in this audience can be emailed.";
  return count === preview.baseCount
    ? `All ${count} ${why}`
    : `${count} of ${preview.baseCount} ${why}`;
}

export default function EligibilityFunnel({
  preview,
  loading,
  error,
  onRetry,
}: {
  preview: AudiencePreview | null;
  loading: boolean;
  error: string | null;
  onRetry: () => void;
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
  const base = Math.max(preview.baseCount, 1);
  const rows: { key: string; label: string; n: number; drop: boolean }[] = [
    { key: "match", label: "Match this audience", n: preview.baseCount, drop: false },
    ...(preview.exclusionCount > 0
      ? [{ key: "exclusion", label: "On the exclusion list", n: preview.exclusionCount, drop: true }]
      : []),
    { key: "invalid", label: "No valid email address", n: preview.excluded.invalid, drop: true },
    { key: "consent", label: "No email consent", n: preview.excluded.noConsent, drop: true },
    { key: "unsub", label: "Unsubscribed", n: preview.excluded.unsubscribed, drop: true },
    { key: "suppressed", label: "Suppressed", n: preview.excluded.suppressed, drop: true },
    { key: "final", label: "Can be emailed", n: preview.finalCount, drop: false },
  ];
  const reason = blockingReason(preview);
  return (
    <div className="grid gap-2" aria-label="Eligibility" data-funnel>
      <ul className="crm-funnel">
        {rows.map((row) => (
          <li key={row.key} data-step={row.key} data-drop={row.drop || undefined}>
            <span>{row.label}</span>
            <span className="crm-funnel-bar" aria-hidden="true">
              <i style={{ width: `${Math.min(100, Math.round((row.n / base) * 100))}%` }} />
            </span>
            <span className="num">
              {row.drop && row.n > 0 ? "−" : ""}
              {row.n}
            </span>
          </li>
        ))}
      </ul>
      {reason !== null && (
        <p className="text-label text-warn" role="status" data-blocked>
          {reason}
        </p>
      )}
    </div>
  );
}
