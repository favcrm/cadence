import { useCallback, useEffect, useId, useRef, useState } from "react";
import { useTranslate } from "../../lib/locale";
import { onPrivateSessionReset } from "../../lib/privateSession";
import { readTeamMembers, type TeamMember } from "../../lib/teamRoster";
import { useMaybeResource } from "../../lib/useResource";
import Button from "../../ui/Button";
import PageState from "../../ui/PageState";
import Select, { type SelectOption } from "../../ui/Select";
import Toast, { type ToastMsg } from "../../ui/Toast";
import ConfirmDialog from "../app-shell/shared/ConfirmDialog";
import DrawerShell from "../app-shell/shared/DrawerShell";
import Field from "../app-shell/shared/Field";
import { EmptyState, ErrorNotice, Loading, Notice } from "../app-shell/shared/States";
import { approverMissing, canEditTypes, documentsViewer } from "../documents/documentsAccess";
import {
  confirmType,
  refusalCode,
  setType,
  type DocumentBaseline,
  type DocumentsResult,
  type DocumentType,
  type DocumentTypeInput,
} from "../documents/documentsClient";
import { documentsProbe } from "../documents/documentsProbe";
import type { Viewer } from "../projects/work";
import "../documents/documents.css";
import "./settings-layout.css";

/**
 * CAD-2286 Settings → Documents: the per-type preferences of the hosted
 * document lifecycle — whether a type needs approval, who approves it, the
 * note recipients see and when a delivered document is archived.
 *
 * The types come from the shared capability probe (`documentsProbe`), never a
 * second read: the page shows what the probe answered and asks it again after
 * a write. Approver names come from the hosted roster (`readTeamMembers`);
 * an unreadable roster is unknown, never "the approver has left".
 *
 * Only the workspace owner edits or confirms a type. A member, a read-only
 * board and a platform operator's session see the page without controls; the
 * hosted workspace refuses a direct write whatever this page renders.
 */

type Translate = (english: string) => string;

/** The form's fields as typed; `approver` is "" for the owner role. */
export interface TypeDraft {
  label: string;
  requireApproval: boolean;
  approver: string;
  recipientNote: string;
  archiveAfterDays: string;
}

export type TypeDraftField = "label" | "approver" | "recipientNote" | "archiveAfterDays";
export type TypeDraftErrors = Partial<Record<TypeDraftField, string>>;

const LABEL_MAX = 40;
const NOTE_MAX = 200;
const APPROVER_MAX = 200;
const DAYS_MAX = 3650;
/** A control character (newlines and tabs included), as the server's `hasControl` reads one. */
const hasControl = (value: string) =>
  Array.from(value).some((ch) => ch.charCodeAt(0) < 0x20 || ch.charCodeAt(0) === 0x7f);

/** The server's type key: trimmed and lower-cased. */
const typeKey = (label: string) => label.trim().toLowerCase();

/**
 * The form's checks, mirroring the hosted `validTypeInput`: a label of 1–40
 * characters once trimmed, a note of at most 200, no control character
 * (newlines included) in either or in the approver, and a whole number of days
 * from 1 to 3650. Lengths count UTF-16 units, as the server's `.length` does.
 * A new label may not repeat an existing type's key, so adding never replaces
 * a type the owner has not opened. `label` is checked only when adding.
 */
export function validateTypeDraft(
  draft: TypeDraft,
  adding: boolean,
  existing: readonly string[],
  t: Translate,
): TypeDraftErrors {
  const errors: TypeDraftErrors = {};
  if (adding) {
    const label = draft.label.trim();
    if (hasControl(draft.label)) errors.label = t("Remove line breaks and other control characters.");
    else if (label.length === 0) errors.label = t("Enter a name for this type.");
    else if (label.length > LABEL_MAX) errors.label = t("Use 40 characters or fewer.");
    else if (existing.some((key) => typeKey(key) === typeKey(label)))
      errors.label = t("This type already exists. Edit it from the list instead.");
  }
  if (hasControl(draft.approver) || draft.approver.trim().length > APPROVER_MAX)
    errors.approver = t("Choose an approver from the list.");
  if (hasControl(draft.recipientNote)) errors.recipientNote = t("Remove line breaks and other control characters.");
  else if (draft.recipientNote.length > NOTE_MAX) errors.recipientNote = t("Use 200 characters or fewer.");
  const days = draft.archiveAfterDays.trim();
  if (!/^\d+$/.test(days) || Number(days) < 1 || Number(days) > DAYS_MAX)
    errors.archiveAfterDays = t("Enter a whole number of days from 1 to 3650.");
  return errors;
}

/** The four preference fields `setType` sends. Call only on a valid draft. */
export function draftInput(draft: TypeDraft): DocumentTypeInput {
  return {
    requireApproval: draft.requireApproval,
    approver: draft.approver.trim() === "" ? null : draft.approver.trim(),
    recipientNote: draft.recipientNote,
    archiveAfterDays: Number(draft.archiveAfterDays.trim()),
  };
}

/** The refusals every type write can meet, before its own fallback. */
function sharedFailure(code: string | null, t: Translate): string | null {
  if (code === "owner_only") return t("Only the workspace owner can change document types.");
  if (code === "owner_required") return t("Only the workspace’s own members can change document types.");
  if (code === "membership_changed") return t("Your access changed. Reload the page and try again.");
  if (code === "capability_unavailable")
    return t("The workspace’s member list couldn’t be read just now, so nothing was changed. Try again later.");
  return null;
}

function confirmFailure(code: string | null, t: Translate): string {
  if (code === "conflict") return t("This type changed since you read it. Review the current values, then confirm.");
  if (code === "not_found") return t("This type no longer exists.");
  if (code === "approver_unavailable")
    return t("This type’s approver is no longer a member. Edit the type to choose another approver.");
  return sharedFailure(code, t) ?? t("Couldn’t confirm this type. Try again.");
}

function saveFailure(code: string | null, t: Translate): string {
  if (code === "invalid_request") return t("The board refused these values. Check each field and try again.");
  if (code === "not_found") return t("This type no longer exists.");
  return sharedFailure(code, t) ?? t("Couldn’t save this type. Try again.");
}

/** Roster rows by subject id; `null` when the roster could not be read (unknown). */
type Roster = { members: TeamMember[] } | null;

function memberLabel(member: TeamMember, t: Translate): string {
  return member.name.trim() || member.email || t("Member");
}

/** The approver as a person can read it. */
function approverText(approver: string | null, roster: Roster, t: Translate): string {
  if (approver === null) return t("Workspace owner");
  if (roster === null) return approver;
  const member = roster.members.find((row) => row.userId === approver);
  return member ? memberLabel(member, t) : t("No longer a member");
}

function daysText(days: number, t: Translate): string {
  return days === 1 ? t("1 day after delivery") : t("{days} days after delivery").replace("{days}", () => String(days));
}

/** A type's values, or the baseline's. The note is plain text: React escapes it. */
function Values({ values, roster }: { values: DocumentBaseline | DocumentType; roster: Roster }) {
  const t = useTranslate();
  return (
    <dl className="document-type-values">
      <div>
        <dt>{t("Approval")}</dt>
        <dd>{values.requireApproval ? t("Required before sharing") : t("Not required")}</dd>
      </div>
      <div>
        <dt>{t("Approver")}</dt>
        <dd data-approver>{approverText(values.approver, roster, t)}</dd>
      </div>
      <div>
        <dt>{t("Archived")}</dt>
        <dd>{daysText(values.archiveAfterDays, t)}</dd>
      </div>
      <div>
        <dt>{t("Recipient note")}</dt>
        <dd data-recipient-note>{values.recipientNote === "" ? t("None") : values.recipientNote}</dd>
      </div>
    </dl>
  );
}

/** One drawer at a time; the token names this opening, as `Members.tsx` does. */
type OpenDrawer = { mode: "add"; token: number } | { mode: "edit"; label: string; token: number };

export default function DocumentTypes({
  hosted,
  viewer,
  viewerSub,
}: {
  /** The lifecycle is hosted only: a native board never reads the probe. */
  hosted: boolean;
  viewer: Viewer;
  /** The board session's subject (`meta.session.user.sub`). */
  viewerSub: string | null;
}) {
  const t = useTranslate();
  const probeState = useMaybeResource(hosted ? documentsProbe : null);
  const answer = probeState?.data ?? null;
  const available = answer?.kind === "available";
  const access = documentsViewer({ sub: viewerSub, operator: viewer.operator, readOnly: viewer.readOnly });
  const canEdit = canEditTypes(access);

  const [roster, setRoster] = useState<Roster>(null);
  const [rosterFailed, setRosterFailed] = useState(false);
  const [rosterAttempt, setRosterAttempt] = useState(0);
  const [open, setOpen] = useState<OpenDrawer | null>(null);
  const openSeq = useRef(0);
  const [confirming, setConfirming] = useState<DocumentType | null>(null);
  const [writing, setWriting] = useState(false);
  const [toast, setToast] = useState<ToastMsg | null>(null);
  const toastTimer = useRef(0);
  const headingRef = useRef<HTMLHeadingElement | null>(null);
  /** Where focus lands once a write settles: the row's Edit button when it is
   *  still there, else the heading. Set after the commit that removes the
   *  dialog or drawer, so it wins over their own return of focus. */
  const [focusAfter, setFocusAfter] = useState<{ label: string | null; n: number }>({ label: null, n: 0 });
  useEffect(() => {
    if (focusAfter.n === 0) return;
    const row =
      focusAfter.label === null
        ? null
        : Array.from(document.querySelectorAll<HTMLElement>("[data-record-id]")).find(
            (element) => element.getAttribute("data-record-id") === focusAfter.label,
          );
    // Edit is the row's last action.
    const button = row?.querySelector<HTMLElement>(".document-type-actions button:last-child");
    (button ?? headingRef.current)?.focus();
  }, [focusAfter]);
  const focusSoon = (label: string | null) => setFocusAfter((current) => ({ label, n: current.n + 1 }));

  const say = useCallback((kind: ToastMsg["kind"], text: string) => {
    setToast({ kind, text });
    window.clearTimeout(toastTimer.current);
    toastTimer.current = window.setTimeout(() => setToast(null), 4200);
  }, []);
  useEffect(() => () => window.clearTimeout(toastTimer.current), []);

  useEffect(() => onPrivateSessionReset(() => {
    setRoster(null);
    setRosterFailed(false);
    setOpen(null);
    setConfirming(null);
    setToast(null);
  }), []);

  // The roster names approvers and lists who may be chosen. Read once the
  // lifecycle is available; any failure (a refusal, a 503, a malformed row)
  // leaves it unknown.
  useEffect(() => {
    if (!available) return;
    const controller = new AbortController();
    readTeamMembers(controller.signal)
      .then((members) => {
        if (controller.signal.aborted) return;
        setRoster({ members });
        setRosterFailed(false);
      })
      .catch(() => {
        if (controller.signal.aborted) return;
        setRoster(null);
        setRosterFailed(true);
      });
    return () => controller.abort();
  }, [available, rosterAttempt]);
  const rereadRoster = () => setRosterAttempt((count) => count + 1);

  /** A write's receipt replaces its row in the shared probe; the probe is then asked again. */
  const applyReceipt = (type: DocumentType) => {
    documentsProbe.mutate((data) => {
      if (data.kind !== "available") return data;
      const rest = data.types.filter((row) => typeKey(row.label) !== typeKey(type.label));
      return { ...data, types: [...rest, type].sort((a, b) => (a.label < b.label ? -1 : a.label > b.label ? 1 : 0)) };
    });
  };
  const reread = () => void documentsProbe.refresh();

  const runConfirm = async (row: DocumentType) => {
    setWriting(true);
    let result: DocumentsResult<DocumentType>;
    try {
      result = await confirmType(row.label, row.rev);
    } catch {
      // An abort (the private session paused): the server may have confirmed
      // the row already, so say nothing and read again.
      setWriting(false);
      setConfirming(null);
      focusSoon(row.label);
      reread();
      return;
    }
    setWriting(false);
    setConfirming(null);
    focusSoon(row.label);
    if (result.ok) {
      applyReceipt(result.value);
      say("ok", t("Document type confirmed."));
      reread();
      return;
    }
    const code = refusalCode(result);
    say("err", confirmFailure(code, t));
    if (code === "approver_unavailable") rereadRoster();
    // A conflict or a missing row means the list is out of date; any other
    // refusal may mean it is too. The page shows what the board holds now.
    reread();
  };

  if (!hosted || answer?.kind === "unavailable") {
    const newer = answer?.kind === "unavailable" && answer.reason === "unsupported_capability";
    return (
      <section className="document-types-page px-4 lg:px-8 py-5 min-w-0 max-w-3xl">
        <PageState title={t("Document types aren’t available here")}>
          {newer
            ? t("This workspace uses a newer version of document review than this page supports. Reload the page to update it.")
            : t("This workspace doesn’t have document review turned on.")}
        </PageState>
      </section>
    );
  }

  if (viewer.operator === null) {
    return (
      <section className="document-types-page px-4 lg:px-8 py-5 min-w-0 max-w-3xl">
        <PageState title={viewer.access === "unavailable" ? t("Access could not be confirmed") : t("Checking access…")}>
          {viewer.access === "unavailable" ? (
            <>
              {t("The board’s access check did not answer, so document types stay closed.")}{" "}
              {viewer.onRetryAccess && (
                <Button size="sm" onClick={viewer.onRetryAccess}>
                  {t("Retry access check")}
                </Button>
              )}
            </>
          ) : (
            t("Checking whether this session may manage document types…")
          )}
        </PageState>
      </section>
    );
  }

  const types = available ? answer.types : [];
  const rosterIds = roster === null ? null : roster.members.map((member) => member.userId);
  const openRow = open?.mode === "edit" ? types.find((row) => row.label === open.label) ?? null : null;

  let body;
  if (!available) {
    // No answer yet, or a failed probe that never answered available: never
    // a baseline the board did not send.
    body =
      answer?.kind === "error" && !probeState?.inFlight ? (
        <ErrorNotice onRetry={() => void documentsProbe.refresh()} retryLabel={t("Retry")}>
          {t("Couldn’t load document types.")}
        </ErrorNotice>
      ) : (
        <Loading>{t("Loading document types…")}</Loading>
      );
  } else {
    body = (
      <>
        {rosterFailed && (
          <Notice state="roster-unavailable" onRetry={rereadRoster} retryLabel={t("Retry")}>
            {t("Couldn’t read the member list, so approvers are shown by their id.")}
          </Notice>
        )}
        <section className="document-types-group" aria-labelledby="document-types-list">
          <h2 id="document-types-list" className="document-types-label">
            {t("Document types")}
          </h2>
          {types.length === 0 ? (
            <EmptyState name="document-types" title={t("No type has its own preferences yet")}>
              {t("Every document uses the default below.")}
            </EmptyState>
          ) : (
            <ul className="settings-rows" aria-label={t("Document types")}>
              {types.map((row) => {
                const pending = row.state === "pending";
                const missing = approverMissing(access, row, rosterIds);
                return (
                  <li key={row.label} data-type-label={row.label}>
                    <div className="settings-row document-type-row" data-record-id={row.label}>
                      <span className="settings-row-text">
                        <span className="document-type-name">
                          <b>{row.label}</b>
                          {pending && (
                            <span className="chip bg-warn/10 text-warn">
                              {canEdit ? t("Needs your confirmation") : t("Waiting for the owner")}
                            </span>
                          )}
                        </span>
                        <Values values={row} roster={roster} />
                        {missing && (
                          <span className="document-type-warn text-label text-warn">
                            {t("This approver is no longer a member, so nobody can approve this type. Edit it to choose another approver.")}
                          </span>
                        )}
                      </span>
                      {canEdit && (
                        <span className="document-type-actions">
                          {pending && (
                            <Button
                              size="sm"
                              variant="primary"
                              aria-label={t("Confirm {label}").replace("{label}", () => row.label)}
                              onClick={() => setConfirming(row)}
                            >
                              {t("Confirm")}
                            </Button>
                          )}
                          <Button
                            size="sm"
                            aria-label={t("Edit {label}").replace("{label}", () => row.label)}
                            onClick={() => setOpen({ mode: "edit", label: row.label, token: ++openSeq.current })}
                          >
                            {t("Edit")}
                          </Button>
                        </span>
                      )}
                    </div>
                  </li>
                );
              })}
            </ul>
          )}
        </section>
        <section className="document-types-group" aria-labelledby="document-types-default">
          <h2 id="document-types-default" className="document-types-label">
            {t("Default for new types")}
          </h2>
          <div className="settings-rows" data-baseline>
            <div className="settings-row">
              <span className="settings-row-text">
                <small>{t("A type uses these until the owner confirms its own preferences.")}</small>
                <Values values={answer.baseline} roster={roster} />
              </span>
            </div>
          </div>
        </section>
      </>
    );
  }

  return (
    <section
      className="document-types-page px-4 lg:px-8 py-5 min-w-0 max-w-3xl"
      aria-labelledby="document-types-title"
      aria-busy={probeState?.inFlight || undefined}
    >
      <header className="document-types-heading">
        <div>
          <h1 id="document-types-title" ref={headingRef} tabIndex={-1} className="text-section font-semibold text-ink-100 focus:outline-none">
            {t("Documents")}
          </h1>
          <p className="mt-1 text-secondary text-ink-400">
            {t("Who approves each type of document, what recipients are told and when it’s archived.")}
          </p>
        </div>
        {canEdit && available && (
          <Button size="sm" onClick={() => setOpen({ mode: "add", token: ++openSeq.current })}>
            {t("Add type")}
          </Button>
        )}
      </header>
      {available && !canEdit && (
        <Notice state="read-only">{t("The workspace owner manages document types.")}</Notice>
      )}
      {body}

      {canEdit && available && open && (open.mode === "add" || openRow) && (
        <TypeDrawer
          key={open.token}
          row={openRow}
          baseline={answer.baseline}
          existing={types.map((row) => row.label)}
          roster={roster}
          onClose={() => setOpen(null)}
          onSaved={(type) => {
            setOpen(null);
            applyReceipt(type);
            focusSoon(type.label);
            say("ok", type.state === "confirmed" ? t("Document type saved.") : t("Saved. It waits for the owner’s confirmation."));
            reread();
          }}
          onUnsettled={(quiet) => {
            setOpen(null);
            focusSoon(openRow?.label ?? null);
            if (!quiet) say("warn", t("Couldn’t confirm the save, so the list was read again."));
            reread();
          }}
          onRosterChanged={rereadRoster}
        />
      )}

      {canEdit && confirming && (
        <ConfirmDialog
          title={t("Confirm document type?")}
          body={
            <>
              <p>
                {t("Confirm “{label}”? These preferences then apply to every document of this type.").replace(
                  "{label}",
                  () => confirming.label,
                )}
              </p>
              <Values values={confirming} roster={roster} />
            </>
          }
          confirmLabel={t("Confirm")}
          confirmVariant="primary"
          pending={writing}
          error={null}
          onCancel={() => {
            if (!writing) setConfirming(null);
          }}
          onConfirm={() => void runConfirm(confirming)}
        />
      )}
      <Toast msg={toast} />
    </section>
  );
}

/**
 * The owner's form for one type. The draft is taken from the row (or the
 * baseline, for a new type) once, when the drawer opens, and lives here: a
 * re-read of the types replaces `row` but never the draft, so typing survives
 * a refresh. If the row changed meanwhile, the form says so.
 */
function TypeDrawer({
  row,
  baseline,
  existing,
  roster,
  onClose,
  onSaved,
  onUnsettled,
  onRosterChanged,
}: {
  /** The type being edited, as the probe holds it now; null when adding. */
  row: DocumentType | null;
  baseline: DocumentBaseline;
  existing: string[];
  roster: Roster;
  onClose: () => void;
  onSaved: (type: DocumentType) => void;
  /** The write may have applied but its answer proves nothing; `quiet` for an
   *  abort, which reports nothing, as the Documents screen does. */
  onUnsettled: (quiet: boolean) => void;
  onRosterChanged: () => void;
}) {
  const t = useTranslate();
  const fieldId = useId();
  const adding = row === null;
  const source = row ?? baseline;
  const [initial] = useState<TypeDraft>(() => ({
    label: row?.label ?? "",
    requireApproval: source.requireApproval,
    approver: source.approver ?? "",
    recipientNote: source.recipientNote,
    archiveAfterDays: String(source.archiveAfterDays),
  }));
  const [openedRev] = useState(row?.rev ?? null);
  const [draft, setDraft] = useState<TypeDraft>(initial);
  const [errors, setErrors] = useState<TypeDraftErrors>({});
  const [formError, setFormError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const labelRef = useRef<HTMLInputElement | null>(null);
  const noteRef = useRef<HTMLInputElement | null>(null);
  const daysRef = useRef<HTMLInputElement | null>(null);
  const dirty = (Object.keys(initial) as (keyof TypeDraft)[]).some((key) => initial[key] !== draft[key]);
  const changed = row !== null && openedRev !== null && row.rev !== openedRev;

  useEffect(() => {
    if (!adding) return;
    const timer = window.setTimeout(() => labelRef.current?.focus(), 0);
    return () => window.clearTimeout(timer);
  }, [adding]);

  const edit = (patch: Partial<TypeDraft>) => {
    setDraft((current) => ({ ...current, ...patch }));
    const fields = Object.keys(patch) as TypeDraftField[];
    if (fields.some((field) => errors[field])) {
      setErrors((current) => {
        const next = { ...current };
        for (const field of fields) delete next[field];
        return next;
      });
    }
  };

  const save = async () => {
    if (busy) return;
    const found = validateTypeDraft(draft, adding, existing, t);
    setErrors(found);
    setFormError(null);
    if (Object.keys(found).length > 0) {
      (found.label ? labelRef : found.recipientNote ? noteRef : found.archiveAfterDays ? daysRef : null)?.current?.focus();
      return;
    }
    setBusy(true);
    let result: DocumentsResult<DocumentType>;
    try {
      result = await setType(adding ? draft.label.trim() : row.label, draftInput(draft));
    } catch {
      // An abort: the server may have saved it, so the list is read again.
      setBusy(false);
      onUnsettled(true);
      return;
    }
    setBusy(false);
    if (result.ok) {
      onSaved(result.value);
      return;
    }
    // An unreadable or malformed success proves nothing either way.
    if (result.failure.kind === "invalid") {
      onUnsettled(false);
      return;
    }
    const code = refusalCode(result);
    if (code === "approver_unavailable") {
      setErrors({ approver: t("That approver is no longer a member. Choose another approver.") });
      onRosterChanged();
      return;
    }
    setFormError(saveFailure(code, t));
  };

  const options: SelectOption[] = [
    { value: "", label: t("Workspace owner") },
    ...(roster?.members ?? []).map((member) => ({
      value: member.userId,
      label: memberLabel(member, t),
      ...(member.email && member.name.trim() ? { hint: member.email } : {}),
    })),
  ];
  // The draft's approver stays choosable even when the roster cannot be read
  // or no longer lists them, so opening the form never changes it unseen.
  if (draft.approver !== "" && !options.some((option) => option.value === draft.approver))
    options.push({
      value: draft.approver,
      label:
        roster === null
          ? draft.approver
          : t("{id} (not a current member)").replace("{id}", () => draft.approver),
    });

  const title = adding ? t("Add document type") : row.label;
  const body = (
    <form
      id={`${fieldId}-form`}
      className="document-type-form"
      noValidate
      onSubmit={(event) => {
        event.preventDefault();
        void save();
      }}
    >
      {changed && (
        <Notice state="changed">
          {t("This type changed after you opened it. Saving replaces it with the values below.")}
        </Notice>
      )}
      {adding && (
        <Field
          label={t("Type name")}
          hint={t("Up to 40 characters. Names are stored in lowercase, so “Brief” and “brief” are the same type.")}
          error={errors.label}
          disabled={busy}
        >
          {(control) => (
            <input
              {...control}
              ref={labelRef}
              className="field"
              type="text"
              autoComplete="off"
              value={draft.label}
              onChange={(event) => edit({ label: event.target.value })}
            />
          )}
        </Field>
      )}
      <label className="document-type-check">
        <input
          type="checkbox"
          checked={draft.requireApproval}
          disabled={busy}
          onChange={(event) => edit({ requireApproval: event.target.checked })}
        />
        <span>{t("Require approval before a document of this type is shared")}</span>
      </label>
      <Field label={t("Approver")} hint={t("Who approves documents of this type.")} error={errors.approver} disabled={busy}>
        {(control) => (
          <Select
            id={control.id}
            full
            aria-label={t("Approver")}
            value={draft.approver}
            disabled={busy}
            options={options}
            onChange={(value) => edit({ approver: value })}
          />
        )}
      </Field>
      <Field
        label={t("Recipient note")}
        hint={t("Shown to recipients as plain text, on one line. Up to 200 characters.")}
        error={errors.recipientNote}
        disabled={busy}
      >
        {(control) => (
          <input
            {...control}
            ref={noteRef}
            className="field"
            type="text"
            autoComplete="off"
            maxLength={NOTE_MAX}
            value={draft.recipientNote}
            onChange={(event) => edit({ recipientNote: event.target.value })}
          />
        )}
      </Field>
      <Field
        label={t("Archive after (days)")}
        hint={t("A delivered document of this type moves to Archived after this many days, from 1 to 3650.")}
        error={errors.archiveAfterDays}
        disabled={busy}
      >
        {(control) => (
          <input
            {...control}
            ref={daysRef}
            className="field num"
            type="text"
            inputMode="numeric"
            autoComplete="off"
            value={draft.archiveAfterDays}
            onChange={(event) => edit({ archiveAfterDays: event.target.value })}
          />
        )}
      </Field>
      {formError && <ErrorNotice bare>{formError}</ErrorNotice>}
    </form>
  );

  const foot =
    adding || dirty ? (
      <div className="document-type-foot">
        <Button variant="ghost" size="sm" disabled={busy} onClick={onClose}>
          {t("Discard")}
        </Button>
        <button type="submit" form={`${fieldId}-form`} className="btn btn-sm btn-primary" disabled={busy} aria-busy={busy || undefined}>
          {busy ? t("Saving…") : t("Save")}
        </button>
      </div>
    ) : null;

  return (
    <DrawerShell
      kind="document-type"
      label={title}
      title={title}
      tabs={[{ id: "details", label: title, panel: body }]}
      foot={foot}
      onClose={onClose}
    />
  );
}
