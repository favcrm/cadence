import { useEffect, useRef, useState } from "react";
import Button from "../../ui/Button";
import { api } from "../../lib/api";
import { fmtTime } from "../../lib/fmt";
import { useLocale } from "../../lib/locale";
import type { HostedLocalDispatchSpawnSpec, IssueAssignmentRow } from "../../lib/types";
import { isolate } from "../decisions/stagedWrite";
import {
  buildCancelPayload,
  buildResumePayload,
  buildRetryPayload,
  canCancelLocalAssignment,
  canResumeLocalAssignment,
  canRetryLocalAssignment,
  makeOperationId,
  VALID_EFFORT_OPTIONS,
} from "./hostedAssignmentControlsModel";

const STATE_CHIP: Record<string, string> = {
  waiting: "bg-ink-800 text-ink-200",
  queued: "bg-info/15 text-info",
  running: "bg-accent/15 text-accent",
  blocked: "bg-warn/15 text-warn",
  result: "bg-ok/15 text-ok",
  failed: "bg-fail/15 text-fail",
  cancelled: "bg-ink-800 text-ink-400",
};

type ExpandedAction = "cancel" | "retry" | "resume" | null;

function assignmentSummary(row: IssueAssignmentRow): string {
  const parts: string[] = [];
  if (row.spawn) {
    parts.push(`${row.spawn.provider} · ${row.spawn.repo}`);
    if (row.spawn.model) parts.push(row.spawn.model);
    if (row.spawn.effort) parts.push(row.spawn.effort);
  } else if (row.resume) {
    parts.push(row.resume.lane_alias ?? row.resume.prior_assignment_id ?? row.resume.runner_id);
    if (row.resume.model) parts.push(row.resume.model);
    if (row.resume.effort) parts.push(row.resume.effort);
  } else if (row.target_agent_id) {
    parts.push(row.target_agent_id);
  } else if (row.target) {
    parts.push(row.target);
  }
  if (typeof row.generation === "number") {
    parts.push(`g${row.generation}`);
  }
  return parts.join(" · ");
}

function errorMessage(error: unknown, fallback: string): string {
  if (error instanceof Error && error.message.trim()) return error.message;
  return fallback;
}

export default function HostedLocalAssignmentCard({
  issueId,
  assignment,
  canLocalDispatch,
  readOnly,
  onMutated,
  onToast,
}: {
  issueId: string;
  assignment: IssueAssignmentRow;
  canLocalDispatch: boolean;
  readOnly: boolean;
  onMutated?: () => void;
  onToast?: (msg: string) => void;
}) {
  const { t } = useLocale();
  const ownerControls = canLocalDispatch && !readOnly;
  const allowCancel = canCancelLocalAssignment(assignment, ownerControls);
  const allowRetry = canRetryLocalAssignment(assignment, ownerControls);
  const allowResume = canResumeLocalAssignment(assignment, ownerControls);

  const [expanded, setExpanded] = useState<ExpandedAction>(null);
  const [note, setNote] = useState("");
  const [model, setModel] = useState(
    assignment.spawn?.model ?? assignment.resume?.model ?? "",
  );
  const [effort, setEffort] = useState<HostedLocalDispatchSpawnSpec["effort"] | "">(
    assignment.spawn?.effort ?? assignment.resume?.effort ?? "",
  );
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [opKey, setOpKey] = useState(() => makeOperationId("op"));
  const inFlightRef = useRef(false);

  useEffect(() => {
    setExpanded(null);
    setNote("");
    setModel(assignment.spawn?.model ?? assignment.resume?.model ?? "");
    setEffort(assignment.spawn?.effort ?? assignment.resume?.effort ?? "");
    setError(null);
    setOpKey(makeOperationId("op"));
  }, [assignment.assignment_id, assignment.generation, assignment.state]);

  const resetOperationKey = () => {
    setOpKey(makeOperationId("op"));
  };

  const openAction = (next: ExpandedAction) => {
    setError(null);
    setExpanded((prev) => (prev === next ? null : next));
    resetOperationKey();
  };

  const submitCancel = async () => {
    if (inFlightRef.current || busy) return;
    inFlightRef.current = true;
    setBusy(true);
    setError(null);
    try {
      const payload = buildCancelPayload(assignment, opKey);
      const res = await api.localDispatchCancel(issueId, payload);
      setExpanded(null);
      onMutated?.();
      onToast?.(
        res.replayed ? t("Local assignment cancellation confirmed") : t("Local assignment cancelled"),
      );
    } catch (err) {
      setError(errorMessage(err, t("Could not cancel local assignment.")));
    } finally {
      inFlightRef.current = false;
      setBusy(false);
    }
  };

  const submitRetry = async () => {
    if (inFlightRef.current || busy) return;
    inFlightRef.current = true;
    setBusy(true);
    setError(null);
    try {
      const payload = buildRetryPayload(assignment, opKey, { note, model, effort });
      const res = await api.localDispatchRetry(issueId, payload);
      setExpanded(null);
      setNote("");
      onMutated?.();
      onToast?.(
        res.replayed ? t("Local assignment retry already queued") : t("Retried local assignment"),
      );
    } catch (err) {
      setError(errorMessage(err, t("Could not retry local assignment.")));
    } finally {
      inFlightRef.current = false;
      setBusy(false);
    }
  };

  const submitResume = async () => {
    if (inFlightRef.current || busy) return;
    inFlightRef.current = true;
    setBusy(true);
    setError(null);
    try {
      const payload = buildResumePayload(assignment, opKey, { note, model, effort });
      const res = await api.localDispatch(issueId, payload);
      setExpanded(null);
      setNote("");
      onMutated?.();
      onToast?.(res.replayed ? t("Lane resume already queued") : t("Queued lane resume"));
    } catch (err) {
      setError(errorMessage(err, t("Could not resume spawned lane.")));
    } finally {
      inFlightRef.current = false;
      setBusy(false);
    }
  };

  const summary = assignmentSummary(assignment);
  const chipStyle = STATE_CHIP[assignment.state] ?? "bg-ink-800 text-ink-300";

  return (
    <section
      className="border border-ink-700 rounded-lg bg-ink-900/60 px-3.5 py-3 grid gap-2.5"
      data-testid="hosted-local-assignment-card"
      aria-label={t("Hosted local assignment")}
    >
      <div className="flex flex-wrap items-center justify-between gap-2">
        <div className="flex flex-wrap items-center gap-2 min-w-0">
          <span className={`chip ${chipStyle}`} data-testid="hosted-assignment-state">
            {t(assignment.state)}
          </span>
          {assignment.action === "resume" && (
            <span className="chip bg-ink-800 text-ink-300">{t("resume")}</span>
          )}
          {summary && (
            <span className="text-secondary text-ink-300 num break-all">
              <bdi>{isolate(summary)}</bdi>
            </span>
          )}
        </div>
        {assignment.updated_at && (
          <span className="text-micro text-ink-400 num">{fmtTime(assignment.updated_at)}</span>
        )}
      </div>

      {assignment.text && (
        <p className="text-secondary text-ink-300 m-0 break-words">
          <bdi>{isolate(assignment.text)}</bdi>
        </p>
      )}

      {(allowCancel || allowRetry || allowResume) && (
        <div className="flex flex-wrap items-center gap-2" data-testid="hosted-assignment-actions">
          {allowRetry && (
            <Button
              size="sm"
              variant="primary"
              className="min-h-[36px]"
              disabled={busy}
              aria-expanded={expanded === "retry"}
              onClick={() => openAction("retry")}
            >
              {t("Retry")}
            </Button>
          )}
          {allowResume && (
            <Button
              size="sm"
              variant="primary"
              className="min-h-[36px]"
              disabled={busy}
              aria-expanded={expanded === "resume"}
              onClick={() => openAction("resume")}
            >
              {t("Resume lane")}
            </Button>
          )}
          {allowCancel && (
            <Button
              size="sm"
              variant="danger"
              className="min-h-[36px]"
              disabled={busy}
              aria-expanded={expanded === "cancel"}
              onClick={() => openAction("cancel")}
            >
              {t("Cancel")}
            </Button>
          )}
        </div>
      )}

      {expanded === "cancel" && allowCancel && (
        <div
          className="border-t border-ink-800 pt-2.5 flex flex-wrap items-center justify-between gap-2"
          data-testid="hosted-assignment-cancel-confirm"
        >
          <p className="text-secondary text-ink-300 m-0">
            {t("Cancel this active assignment and clear the ticket lock?")}
          </p>
          <div className="flex flex-wrap items-center gap-2">
            <Button
              size="sm"
              variant="danger"
              className="min-h-[36px]"
              loading={busy}
              disabled={busy}
              onClick={() => void submitCancel()}
            >
              {t("Confirm cancel")}
            </Button>
            <Button
              size="sm"
              variant="secondary"
              className="min-h-[36px]"
              disabled={busy}
              onClick={() => setExpanded(null)}
            >
              {t("Keep")}
            </Button>
          </div>
        </div>
      )}

      {(expanded === "retry" && allowRetry) || (expanded === "resume" && allowResume) ? (
        <form
          className="border-t border-ink-800 pt-2.5 grid gap-2.5"
          data-testid={
            expanded === "retry" ? "hosted-assignment-retry-form" : "hosted-assignment-resume-form"
          }
          onSubmit={(event) => {
            event.preventDefault();
            if (expanded === "retry") {
              void submitRetry();
            } else {
              void submitResume();
            }
          }}
        >
          <label className="grid gap-1">
            <span className="slabel">
              {expanded === "retry"
                ? t("Retry guidance (optional)")
                : t("Follow-up instructions (optional)")}
            </span>
            <textarea
              rows={2}
              className="input w-full"
              placeholder={
                expanded === "retry"
                  ? t("Updated note for this retry…")
                  : t("Instructions to continue in the existing worktree…")
              }
              value={note}
              disabled={busy}
              onChange={(event) => {
                setNote(event.target.value);
                resetOperationKey();
              }}
            />
          </label>

          {(expanded === "resume" || Boolean(assignment.spawn)) && (
            <div className="grid grid-cols-1 sm:grid-cols-2 gap-2">
              <label className="grid gap-1">
                <span className="slabel">{t("Model override (optional)")}</span>
                <input
                  type="text"
                  className="input w-full min-h-[36px]"
                  placeholder={assignment.spawn?.model ?? assignment.resume?.model ?? t("Keep current model")}
                  value={model}
                  disabled={busy}
                  onChange={(event) => {
                    setModel(event.target.value);
                    resetOperationKey();
                  }}
                />
              </label>
              <label className="grid gap-1">
                <span className="slabel">{t("Effort override (optional)")}</span>
                <select
                  className="input w-full min-h-[36px]"
                  value={effort}
                  disabled={busy}
                  onChange={(event) => {
                    setEffort(event.target.value as HostedLocalDispatchSpawnSpec["effort"] | "");
                    resetOperationKey();
                  }}
                >
                  <option value="">{t("Keep current effort")}</option>
                  {VALID_EFFORT_OPTIONS.map((opt) => (
                    <option key={opt} value={opt}>
                      {opt}
                    </option>
                  ))}
                </select>
              </label>
            </div>
          )}

          <div className="flex flex-wrap items-center gap-2">
            <Button
              type="submit"
              size="sm"
              variant="primary"
              className="min-h-[36px]"
              loading={busy}
              disabled={busy}
            >
              {expanded === "retry" ? t("Confirm retry") : t("Send resume")}
            </Button>
            <Button
              size="sm"
              variant="secondary"
              className="min-h-[36px]"
              disabled={busy}
              onClick={() => setExpanded(null)}
            >
              {t("Dismiss")}
            </Button>
          </div>
        </form>
      ) : null}

      {error && (
        <p role="alert" className="text-secondary text-fail m-0">
          {error}
        </p>
      )}
    </section>
  );
}
