import type { AssignmentContext } from "../../lib/api";
import { activityTimeMs } from "../../lib/fmt";
import { useLocale } from "../../lib/locale";

/** Only recorded handoff facts; opening/editing an issue never resets its byline. */
export default function AssignmentSummary({ assignment, full = false }: {
  assignment: AssignmentContext | null;
  full?: boolean;
}) {
  const { t, formatRelativeTime } = useLocale();
  if (!assignment || typeof assignment.actor !== "string" || !assignment.actor.trim() ||
      typeof assignment.at !== "string" || !activityTimeMs(assignment.at) ||
      (assignment.kind !== "assigned" && assignment.kind !== "initial_owner")) {
    return <span>{t(full ? "Unavailable — no recorded handoff." : "assignment details unavailable")}</span>;
  }
  const actor = assignment.actor;
  const label = assignment.kind === "initial_owner"
    ? t("Created by {actor} — initial owner").replace("{actor}", actor)
    : assignment.self_assigned === true
      ? t("Self-assigned")
      : t(full ? "Assigned by {actor} to you" : "by {actor}").replace("{actor}", actor);
  return <span>{label} <time dateTime={assignment.at} title={assignment.at}>· {formatRelativeTime(assignment.at)}</time></span>;
}
