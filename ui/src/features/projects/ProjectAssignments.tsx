import { activityTimeMs, fmtTime } from "../../lib/fmt";
import { useLocale } from "../../lib/locale";

/** Participant-scoped coordination counts; never a count of live processes. */
export function ProjectAssignments({ value }: { value: unknown }) {
  const { t, formatNumber } = useLocale();
  if (!value || typeof value !== "object" || Array.isArray(value)) {
    return <span>{t("Local assignment status is unavailable.")}</span>;
  }
  const summary = value as Record<string, unknown>;
  if (summary.version !== "hosted-cadence-local-assignment-read.v1" ||
      summary.source !== "local_assignment_coordinator" || summary.scope !== "participant" ||
      summary.available !== true || typeof summary.live !== "number" ||
      !Number.isSafeInteger(summary.live) || summary.live < 0 || typeof summary.truncated !== "boolean") {
    return <span>{t("Local assignment status is unavailable.")}</span>;
  }
  const count = formatNumber(summary.live);
  const at = typeof summary.observed_at === "string" && activityTimeMs(summary.observed_at)
    ? summary.observed_at
    : null;
  return <span>
    {t(summary.truncated
      ? "At least {count} open local assignments visible to you"
      : "{count} open local assignments visible to you").replace("{count}", count)}
    {at && ` · ${t("Observed")} ${fmtTime(at)}`}
    {` · ${t("Execution not verified")}`}
  </span>;
}
