import { activityTimeMs, fmtTime } from "../../lib/fmt";
import { useLocale } from "../../lib/locale";

const STATES: Record<string, string> = {
  starting: "Assignment recorded; start not confirmed",
  working: "Started; completion not yet observed",
  finished: "Finished",
  failed: "Failed",
  parked: "Parked",
  refused: "Refused",
  no_report: "Ended without a report",
  stopped: "Stopped",
};

/** Facts from the hosted watcher, not a liveness claim or an approval. */
export function HostedRun({ value }: { value: unknown }) {
  const { t } = useLocale();
  if (!value || typeof value !== "object" || Array.isArray(value)) {
    return <span>{t("Hosted run status is unavailable or not recorded.")}</span>;
  }
  const run = value as Record<string, unknown>;
  const state = typeof run.state === "string" && Object.hasOwn(STATES, run.state)
    ? STATES[run.state]
    : undefined;
  if (!state) return <span>{t("Hosted run status is unavailable or not recorded.")}</span>;
  const name = typeof run.agent_name === "string" && run.agent_name.trim()
    ? run.agent_name
    : null;
  const at = typeof run.ended_at === "string" && activityTimeMs(run.ended_at)
    ? run.ended_at
    : typeof run.started_at === "string" && activityTimeMs(run.started_at)
      ? run.started_at
      : null;
  return (
    <span>
      {name ?? t("Agent name unavailable")} · {t(state)}
      {at && ` · ${fmtTime(at)}`}
      {run.stop_owed === true && ` · ${t("Stop not confirmed")}`}
    </span>
  );
}
