import { TONE_BAR, TONE_CHIP, type HealthView, type ProgressView } from "./work";

/** The health chip: on track / at risk / stalled. */
export function HealthBadge({ view }: { view: HealthView }) {
  return (
    <span className={`chip ${TONE_CHIP[view.tone]}`} data-health={view.state}>
      <i className="w-1.5 h-1.5 rounded-full bg-current" aria-hidden />
      {view.label}
    </span>
  );
}

/** Each reason an epic or milestone is off track, with who acts next and how. */
export function HealthReasons({ view }: { view: HealthView }) {
  if (view.reasons.length === 0) return null;
  return (
    <details className="health-details">
      <summary className="text-label text-warn cursor-pointer">{view.reasons.length} {view.reasons.length === 1 ? "reason" : "reasons"} to check</summary>
    <ul className="space-y-1.5 mt-3" aria-label="health reasons">
      {view.reasons.map((r, i) => (
        <li key={i} className="text-label min-w-0">
          <span className={`break-words ${view.tone === "fail" ? "text-fail" : "text-warn"}`}>{r.detail}</span>
          <span className="block text-ink-400 break-words">
            next{r.owner ? ` (${r.owner})` : ""}: {r.next}
          </span>
        </li>
      ))}
    </ul>
    </details>
  );
}

/** Size-weighted progress bar with its figures. */
export function ProgressBar({ view, tone }: { view: ProgressView; tone: HealthView["tone"] }) {
  return (
    <div className="min-w-0">
      <div className="flex items-center justify-between gap-2 text-micro text-ink-500">
        <span className="num">{view.label}</span>
        <span className="num">{view.percent === null ? "—" : `${view.percent}%`}</span>
      </div>
      <div
        className="h-1.5 rounded bg-ink-800 mt-1 overflow-hidden"
        role="progressbar"
        aria-label="size-weighted progress"
        aria-valuemin={0}
        aria-valuemax={100}
        aria-valuenow={view.percent ?? 0}
      >
        <div className={`h-full ${TONE_BAR[tone]}`} style={{ width: `${view.percent ?? 0}%` }} />
      </div>
    </div>
  );
}
