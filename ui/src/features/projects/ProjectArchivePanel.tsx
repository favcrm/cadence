import { useEffect, useMemo, useRef, useState } from "react";
import { api } from "../../lib/api";
import { ApiError } from "../../lib/apiError";
import { privateSessionGeneration } from "../../lib/privateSession";
import { resources } from "../../lib/resources";
import { useQuery } from "../../lib/useResource";
import { useLocale } from "../../lib/locale";
import { ResourceGate, StaleChip } from "../../ui/ResourceStatus";
import ConfirmDialog from "../app-shell/shared/ConfirmDialog";
import { readLifecycle, receiptMatches, type ArchiveGateState, type ArchiveView } from "./archiveContract";

/**
 * CAD-2255: a project's archive state and its two transitions.
 *
 * The lifecycle receipt is the only authority shown here. A route the server
 * does not serve, another capability, or a shape this page cannot prove is
 * `unavailable`/`unknown`/`unreadable` — never "active", and never a
 * disabled-looking success. Every one of those states is unverified and
 * fails closed for project mutations: the policy editor stays locked, because
 * a 501 or a foreign capability proves only that this endpoint is absent, not
 * that storage never held an archived project (a rollback). A transition
 * reports success only after its own answer proves the intended state at a
 * revision newer than the fence the request pinned; the exact project key is
 * typed in the shared confirm dialog, and the expected revision is the last
 * read this page holds.
 *
 * The policy editor beside this panel is therefore unlocked only by a proved
 * ACTIVE receipt, and is also locked while a confirm is open, a transition is
 * in flight, or the person has unsaved edits.
 */
export default function ProjectArchivePanel({
  project,
  block,
  policyBusy,
  unsavedPolicyEdits,
  onActivity,
  onArchiveState,
  onToast,
}: {
  project: string;
  /** Why this session may not archive or restore, or null when it may. */
  block: string | null;
  /** A policy save or grant check is in flight: no transition may start. */
  policyBusy: boolean;
  /** The person changed the policy (a real edit, not the default proposal). */
  unsavedPolicyEdits: boolean;
  /** True while a confirm is open or a transition is in flight. */
  onActivity: (active: boolean) => void;
  /** What the read proves about policy mutation: archived/active/unknown/unavailable. */
  onArchiveState: (state: ArchiveGateState) => void;
  onToast: (text: string) => void;
}) {
  const { t } = useLocale();
  const resource = resources.projectLifecycle(project);
  const state = useQuery(resource);
  const view: ArchiveView = useMemo(
    () => readLifecycle(state.data, project),
    [state.data, project],
  );
  const [intent, setIntent] = useState<null | "archive" | "restore">(null);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  // A duplicate click before the next render must not start a second write.
  const inFlight = useRef(false);

  // The opening that is live now, readable from a confirmation that started
  // earlier. A project change retires every pending dialogue.
  const projectRef = useRef({ key: project, epoch: 0 });
  if (projectRef.current.key !== project) {
    projectRef.current = { key: project, epoch: projectRef.current.epoch + 1 };
  }

  const archived = view.kind === "available" ? view.receipt.archived : false;
  // A cached active receipt is not proof while a refresh is in flight: another
  // tab may have archived the project since that answer.
  const lifecycleCurrent =
    state.data !== null &&
    !state.inFlight &&
    (state.status === "ok" || state.status === "empty");
  const reported: ArchiveGateState = !lifecycleCurrent
    ? "unknown"
    : view.kind === "available"
      ? (view.receipt.archived ? "archived" : "active")
      : view.kind === "unavailable"
        ? "unavailable"
        : "unknown";
  useEffect(() => {
    onArchiveState(reported);
  }, [reported, onArchiveState]);

  useEffect(() => {
    setIntent(null);
    setError(null);
    setBusy(false);
    inFlight.current = false;
    onActivity(false);
  }, [project, onActivity]);

  const current = () => {
    const epoch = projectRef.current.epoch;
    const generation = privateSessionGeneration();
    return () => projectRef.current.epoch === epoch && privateSessionGeneration() === generation;
  };

  function open(next: "archive" | "restore") {
    if (block || busy || policyBusy || unsavedPolicyEdits || state.inFlight || state.status === "stale") return;
    if (view.kind !== "available") return;
    setError(null);
    setIntent(next);
    onActivity(true);
  }

  function close() {
    if (busy) return;
    setIntent(null);
    setError(null);
    onActivity(false);
  }

  async function confirm() {
    if (intent === null || busy || inFlight.current || block || unsavedPolicyEdits) return;
    if (state.inFlight || state.status === "stale") {
      setError(t("The archive state is being rechecked. Nothing was sent — wait for the current answer."));
      return;
    }
    // The newest read, not the render the dialog opened in.
    const latest = readLifecycle(resource.get().data, project);
    if (latest.kind !== "available") {
      setError(t("The archive state can no longer be proved. Nothing was sent — read it again."));
      void resource.refresh();
      return;
    }
    const target = intent === "archive";
    if (latest.receipt.archived !== !target) {
      setError(t("The project's archive state changed while this dialog was open. Nothing was sent."));
      void resource.refresh();
      return;
    }
    const expectedRevision = latest.receipt.revision;
    const stillCurrent = current();
    inFlight.current = true;
    let finished = false;
    setBusy(true);
    setError(null);
    try {
      const raw =
        target
          ? await api.archiveProject(project, { confirmation: project, expected_revision: expectedRevision })
          : await api.restoreProject(project, { confirmation: project, expected_revision: expectedRevision });
      if (!stillCurrent()) return;
      const receipt = readLifecycle(raw, project);
      if (!receiptMatches(receipt, { project, archived: target, expectedRevision })) {
        throw new Error(t("The board answered without a receipt that proves this change."));
      }
      finished = true;
      resource.write(() => raw);
      setIntent(null);
      onToast(t(target ? "Project archived." : "Project restored."));
      void resource.refresh();
      void resources.projects.refresh();
      void resources.archivedProjects.invalidate();
      void resources.projectIssues(project).invalidate();
    } catch (e) {
      if (!stillCurrent()) return;
      setError(archiveFailure(e, t));
      // A refusal is about a record this page may no longer hold.
      void resource.refresh();
    } finally {
      if (stillCurrent()) {
        inFlight.current = false;
        setBusy(false);
        // An open dialog keeps the policy editor locked until it closes.
        onActivity(!finished);
      }
    }
  }

  const heading = t("Archive");

  return (
    <section className="card mt-4 max-w-2xl p-5" aria-label={heading}>
      <div className="flex flex-wrap items-center gap-2">
        <div className="slabel">{heading}</div>
        {view.kind === "available" && (
          <span className={`chip ${archived ? "bg-warn/10 text-warn" : "bg-ink-800 text-ink-300"}`}>
            {t(archived ? "Archived" : "Active")}
          </span>
        )}
        <StaleChip state={state} />
      </div>
      <p className="mt-2 text-label text-ink-400">
        {t("Archiving keeps the project's work and history and closes it to normal work. Restoring reopens it.")}
      </p>

      {state.data === null ? (
        <ResourceGate
          state={state}
          loading={t("Reading the archive state…")}
          failed={t("The archive state could not be read")}
          onRetry={() => void resource.refresh()}
        />
      ) : view.kind === "unavailable" ? (
        <p className="mt-3 text-secondary text-ink-400" role="status">
          {t("Project archive is unavailable on this server. Access changes stay locked here until the backend serves the archive contract.")}
          <span className="mt-1 block text-label text-ink-500">{view.reason}</span>
        </p>
      ) : view.kind === "unknown" ? (
        <div className="mt-3" role="status">
          <p className="text-secondary text-warn">
            {t("The archive state could not be proved on this server, so access changes stay locked until it answers.")}
          </p>
          <p className="mt-1 text-label text-ink-500">{view.reason}</p>
          <button type="button" className="btn mt-3" onClick={() => void resource.refresh()}>
            {t("Read again")}
          </button>
        </div>
      ) : view.kind === "unreadable" ? (
        <div className="mt-3" role="alert">
          <p className="text-secondary text-fail">
            {t("This board answered with an archive state this page cannot prove, so it is not shown or changed.")}
          </p>
          <p className="mt-1 text-label text-ink-500">{view.reason}</p>
          <button type="button" className="btn mt-3" onClick={() => void resource.refresh()}>
            {t("Read again")}
          </button>
        </div>
      ) : (
        <>
          <dl className="mt-3 grid gap-1 text-label text-ink-400">
            <div>
              <dt className="inline">{t("Last change")}: </dt>
              <dd className="inline text-ink-300">
                {view.receipt.changed_at === null
                  ? t("never")
                  : `${view.receipt.changed_at}${view.receipt.changed_by ? ` · ${view.receipt.changed_by}` : ""}`}
              </dd>
            </div>
            {/* Last-archive evidence stays after a restore; it is not current state. */}
            {view.receipt.archived_at !== null && (
              <div>
                <dt className="inline">{t("Last archived")}: </dt>
                <dd className="inline text-ink-300">
                  {`${view.receipt.archived_at}${view.receipt.archived_by ? ` · ${view.receipt.archived_by}` : ""}`}
                </dd>
              </div>
            )}
          </dl>
          {archived && (
            <p className="mt-3 text-label text-warn" role="status">
              {t("This project is archived. Its work and settings are read-only until it is restored.")}
            </p>
          )}
          {view.receipt.blockers.length > 0 && (
            <div className="mt-3 text-label text-warn" role="status">
              <p>{t("The server lists these reasons it cannot archive this project right now:")}</p>
              <ul className="mt-1 list-disc pl-5">
                {view.receipt.blockers.map((reason) => (
                  <li key={reason}>{reason}</li>
                ))}
              </ul>
            </div>
          )}
          {(state.status === "stale" || state.inFlight) && (
            <p className="mt-3 text-label text-warn">
              {t("This archive state is being rechecked. Wait for the current answer before archiving or restoring.")}
            </p>
          )}
          {block && <p className="mt-3 text-label text-ink-400">{block}</p>}
          {unsavedPolicyEdits && (
            <p className="mt-3 text-label text-warn" role="status">
              {t("Save or discard your access changes before archiving or restoring.")}
            </p>
          )}
          {error && (
            <p className="mt-3 text-secondary text-fail" role="alert">
              {error}
            </p>
          )}
          {!block && (
            <div className="mt-4 flex flex-wrap items-center gap-2">
              {archived ? (
                <button
                  type="button"
                  className="btn btn-primary"
                  onClick={() => open("restore")}
                  disabled={busy || policyBusy || unsavedPolicyEdits || state.inFlight || state.status === "stale"}
                >
                  {t("Restore project…")}
                </button>
              ) : (
                <button
                  type="button"
                  className="btn"
                  onClick={() => open("archive")}
                  disabled={busy || policyBusy || unsavedPolicyEdits || state.inFlight || state.status === "stale" || view.receipt.blockers.length > 0}
                >
                  {t("Archive project…")}
                </button>
              )}
              {state.status === "stale" && (
                <button type="button" className="btn btn-ghost" onClick={() => void resource.refresh()}>
                  {t("Read again")}
                </button>
              )}
            </div>
          )}
        </>
      )}

      {intent !== null && (
        <ConfirmDialog
          title={t(intent === "archive" ? "Archive project" : "Restore project")}
          body={
            intent === "archive"
              ? t("This keeps the project's work and history and closes it to normal work. Type the exact project key to archive it.")
              : t("This reopens the project for work. Type the exact project key to restore it.")
          }
          expect={project}
          match={project}
          inputLabel={t("Type the exact project key to confirm")}
          inputPlaceholder={project}
          inputMode="text"
          confirmLabel={t(intent === "archive" ? "Archive project" : "Restore project")}
          pending={busy}
          error={error}
          onConfirm={() => void confirm()}
          onCancel={close}
        />
      )}
    </section>
  );
}

/** What a refused or unproved transition says. Never a success line. */
function archiveFailure(error: unknown, t: (english: string) => string): string {
  if (error instanceof ApiError) {
    if (error.status === 403) {
      return t("Only the project's owner or the workspace operator can change its archive state.");
    }
    if (error.code === "stale_revision" || error.code === "revision_conflict" || error.status === 409) {
      return t("The archive state changed since this page read it. Nothing was changed — the current state is being reloaded.");
    }
    if (error.code === "confirmation_mismatch") {
      return t("The confirmation did not match the project key. Nothing was changed.");
    }
    if (error.code === "archive_blocked" || error.code === "blocked") {
      return t("The server refused to archive this project. Nothing was changed; its reasons are shown above.");
    }
    if (error.status === 501 || error.status === 503) {
      return t("This server does not offer project archive. Nothing was changed.");
    }
    return error.message;
  }
  return error instanceof Error ? error.message : t("The archive change could not be confirmed.");
}
