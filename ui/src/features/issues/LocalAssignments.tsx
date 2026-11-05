import { activityTimeMs, fmtTime } from "../../lib/fmt";
import { useLocale } from "../../lib/locale";

const STATES: Record<string, string> = {
  waiting: "Waiting for assignment",
  queued: "Queued for claim",
  running: "Claim recorded; execution not confirmed",
  blocked: "Worker reported blocked",
  failed: "Worker reported failure",
  result: "Result reported; acceptance not confirmed",
  cancelled: "Assignment cancelled; stop not confirmed",
};

function object(value: unknown): Record<string, unknown> | null {
  return value && typeof value === "object" && !Array.isArray(value)
    ? value as Record<string, unknown>
    : null;
}

function hasVisibleState(value: unknown, states: readonly string[]): boolean {
  const projection = object(value);
  if (projection?.version !== "hosted-cadence-local-assignment-read.v1" ||
      projection.source !== "local_assignment_coordinator" || projection.scope !== "participant" ||
      projection.available !== true || !Array.isArray(projection.assignments)) return false;
  return projection.assignments.some((item) => {
    const row = object(item);
    return row !== null && typeof row.state === "string" && states.includes(row.state);
  });
}

/** A visible open receipt is a coordination fact, not proof of execution. */
export function hasVisibleOpenAssignment(value: unknown): boolean {
  return hasVisibleState(value, ["waiting", "queued", "running", "blocked"]);
}

export function hasVisibleReportedResult(value: unknown): boolean {
  return hasVisibleState(value, ["result"]);
}

/** `kind` is absent only for a dev row in this versioned projection. */
export function hasVisibleDevAssignment(value: unknown): boolean {
  const projection = object(value);
  if (projection?.version !== "hosted-cadence-local-assignment-read.v1" ||
      projection.source !== "local_assignment_coordinator" || projection.scope !== "participant" ||
      projection.available !== true || !Array.isArray(projection.assignments)) return false;
  return projection.assignments.some((item) => {
    const row = object(item);
    return row !== null && typeof row.assignment_id === "string" && typeof row.state === "string" &&
      Object.hasOwn(STATES, row.state) && (row.kind === undefined || row.kind === "dev");
  });
}

export function LocalAssignmentBadge({ value }: { value: unknown }) {
  const { t } = useLocale();
  if (value === undefined) return null;
  const projection = object(value);
  if (projection?.version !== "hosted-cadence-local-assignment-read.v1" ||
      projection.source !== "local_assignment_coordinator" || projection.scope !== "participant" ||
      projection.available !== true || !Array.isArray(projection.assignments)) {
    return <span>{t("Local assignment status is unavailable.")}</span>;
  }
  const row = object(projection.assignments[0]);
  if (!row) return projection.truncated === true
    ? <span>{t("Additional visible assignments are not shown.")}</span>
    : null;
  const state = typeof row.state === "string" && Object.hasOwn(STATES, row.state)
    ? STATES[row.state]
    : null;
  return <span>{t(state ?? "Local assignment status is unavailable.")}</span>;
}

/** Participant-visible coordination receipts; these do not prove live execution. */
export function LocalAssignments({ value }: { value: unknown }) {
  const { t } = useLocale();
  const projection = object(value);
  if (projection?.version !== "hosted-cadence-local-assignment-read.v1" ||
      projection.source !== "local_assignment_coordinator" || projection.scope !== "participant" ||
      projection.available !== true || !Array.isArray(projection.assignments)) {
    return <span>{t("Local assignment status is unavailable.")}</span>;
  }
  const rows = projection.assignments.map(object);
  if (rows.some((row) => !row || typeof row.assignment_id !== "string" ||
      typeof row.state !== "string" || !Object.hasOwn(STATES, row.state))) {
    return <span>{t("Local assignment status is unavailable.")}</span>;
  }
  if (rows.length === 0) {
    return <span>{t("No local assignments are visible to you; others may exist.")}</span>;
  }
  return (
    <div className="grid gap-2">
      {rows.map((row) => {
        // Validated above; keep raw identities and assignment ids out of the UI.
        const assignment = row!;
        const state = assignment.state as string;
        const updated = typeof assignment.updated_at === "string" && activityTimeMs(assignment.updated_at)
          ? assignment.updated_at
          : null;
        const deliverable = object(assignment.deliverable);
        const check = object(assignment.deliverable_check);
        let link: string | null = null;
        if (deliverable?.type === "link" && typeof deliverable.ref === "string") {
          try {
            const url = new URL(deliverable.ref);
            if (url.protocol === "https:" && !url.username && !url.password) link = url.href;
          } catch { /* A malformed link is not presented as navigable. */ }
        }
        return (
          <div key={assignment.assignment_id as string}>
            <div>{t(STATES[state])}{updated && ` · ${fmtTime(updated)}`}</div>
            {typeof assignment.text === "string" && assignment.text.trim() && (
              <div className="whitespace-pre-wrap">{assignment.text}</div>
            )}
            {deliverable?.type === "artifact" && typeof deliverable.ref === "string" && (
              <div>
                {deliverable.ref} · {t(check?.found === true
                  ? "Available in Evidence"
                  : check?.found === false
                    ? "Artifact not found on this issue"
                    : "Artifact availability could not be verified")}
              </div>
            )}
            {deliverable?.type === "link" && (link
              ? <a href={link} target="_blank" rel="noopener noreferrer">{t("Open reported link")}</a>
              : <div>{t("Reported link is unavailable.")}</div>)}
          </div>
        );
      })}
      <span className="dim">{t("Only assignments visible to you are shown; execution liveness is not verified.")}</span>
      {projection.truncated === true && <span className="dim">{t("Additional visible assignments are not shown.")}</span>}
    </div>
  );
}
