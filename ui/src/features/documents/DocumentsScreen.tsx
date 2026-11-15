import { useCallback, useEffect, useRef, useState } from "react";
import { useLocale, useTranslate } from "../../lib/locale";
import { onPrivateSessionReset, privateReadsAllowed } from "../../lib/privateSession";
import type { DocumentStageFilter, Route } from "../../lib/router";
import { useMaybeResource } from "../../lib/useResource";
import Button from "../../ui/Button";
import Link from "../../ui/Link";
import PageState from "../../ui/PageState";
import SectionTabs from "../../ui/SectionTabs";
import Toast, { type ToastMsg } from "../../ui/Toast";
import ConfirmDialog from "../app-shell/shared/ConfirmDialog";
import DataTable, { type DataTableColumn } from "../app-shell/shared/DataTable";
import { EmptyState, ErrorNotice, Loading } from "../app-shell/shared/States";
import { ARTIFACT_KINDS, canvasSnapshot, resetCanvas, type ArtifactKind, type ArtifactRef } from "../canvas/canvasStore";
import { openRecordedArtifact } from "../threads/WorkContext";
import { canApprove, canArchive, canRestore, typeFor, type DocumentsViewer } from "./documentsAccess";
import {
  approve,
  archive,
  list,
  refusalCode,
  restore,
  type DocumentRow,
  type DocumentsResult,
  type DocumentStage,
  type DocumentType,
} from "./documentsClient";
import { documentsProbe } from "./documentsProbe";
import "./documents.css";

/**
 * CAD-2286: the Documents screen on a hosted board with the document
 * lifecycle. Active and Archived tabs, the Active tab's stage chips (in the
 * URL, filtered here over the server's list), and the review actions.
 *
 * The probe is App's: it probes the shared store, and this screen only reads
 * it, asking again after a refusal that may mean the types changed. Every
 * control here only hides; the hosted workspace decides each action.
 */

/** The canvas conversation this screen binds to open a file. The canvas
 *  renders nothing without one, and `composerKey: null` turns Ask to change
 *  off. The hosted bytes route ignores it. */
export const DOCUMENTS_CONVERSATION = "documents:screen";

type Translate = (english: string) => string;
type Write = "approve" | "archive" | "restore";
type Pending = { action: Write; row: DocumentRow; version: number };

const STAGE_LABEL: Record<DocumentStage, string> = {
  draft: "Draft",
  in_review: "In review",
  approved: "Approved",
  delivered: "Delivered",
  archived: "Archived",
};

/** The existing chip palette: warn asks for attention, accent is done, ink is quiet. */
const STAGE_CHIP: Record<DocumentStage, string> = {
  draft: "chip bg-ink-800 text-ink-400",
  in_review: "chip bg-warn/10 text-warn",
  approved: "chip bg-accent/10 text-accent",
  delivered: "chip bg-info/10 text-info",
  archived: "chip bg-ink-800 text-ink-500",
};

/** The Active tab's chips, in the order operator decision 1 names them. */
const STAGE_FILTERS: { stage: DocumentStageFilter | null; label: string }[] = [
  { stage: null, label: "All" },
  { stage: "in_review", label: "In review" },
  { stage: "approved", label: "Approved" },
  { stage: "delivered", label: "Delivered" },
  { stage: "draft", label: "Draft" },
];

const isArtifactKind = (kind: string): kind is ArtifactKind => (ARTIFACT_KINDS as readonly string[]).includes(kind);

/** The refusals every write can meet, before its own fallback (as `Members.tsx`'s `sharedFailure`). */
function sharedFailure(code: string | null, t: Translate): string | null {
  if (code === "not_found") return t("This document no longer exists.");
  if (code === "owner_required") return t("Only the workspace’s own members can change documents.");
  if (code === "membership_changed") return t("Your access changed. Reload the page and try again.");
  return null;
}

function approveFailure(code: string | null, t: Translate): string {
  if (code === "conflict") return t("This document changed since you opened it. Review the new version, then approve it.");
  if (code === "not_in_review") return t("This document is no longer in review.");
  if (code === "approver_unavailable")
    return t("This type’s approver has left the workspace. The owner must update the type before it can be approved.");
  if (code === "not_approver") return t("You’re not the approver for this type of document.");
  if (code === "self_approval") return t("You can’t approve a version you wrote.");
  if (code === "document_archived") return t("This document has been archived.");
  return sharedFailure(code, t) ?? t("Couldn’t approve this document. Try again.");
}

function archiveFailure(code: string | null, t: Translate): string {
  if (code === "owner_only") return t("Only the workspace owner can archive documents.");
  return sharedFailure(code, t) ?? t("Couldn’t archive this document. Try again.");
}

function restoreFailure(code: string | null, t: Translate): string {
  if (code === "owner_only") return t("Only the workspace owner can restore documents.");
  return sharedFailure(code, t) ?? t("Couldn’t restore this document. Try again.");
}

export default function DocumentsScreen({
  hosted,
  viewer,
  archived,
  stage,
  hrefFor,
}: {
  /** The lifecycle is hosted only: a native board never reads the probe. */
  hosted: boolean;
  viewer: DocumentsViewer;
  /** The Archived tab (`?archived=1`). */
  archived: boolean;
  /** The Active tab's stage filter (`?stage=`). */
  stage?: DocumentStageFilter;
  hrefFor: (route: Route) => string;
}) {
  const t = useTranslate();
  const { formatDate } = useLocale();
  const probeState = useMaybeResource(hosted ? documentsProbe : null);
  const answer = probeState?.data ?? null;
  const available = answer?.kind === "available";
  const types: DocumentType[] = answer?.kind === "available" ? answer.types : [];

  /** The rows of one tab; `null` until that tab's first list answers. */
  const [rows, setRows] = useState<{ archived: boolean; rows: DocumentRow[] } | null>(null);
  const [loading, setLoading] = useState(false);
  const [failed, setFailed] = useState(false);
  /** A list answered 404 after the probe said available: the lifecycle was turned off. */
  const [gone, setGone] = useState(false);
  const [attempt, setAttempt] = useState(0);
  const [confirming, setConfirming] = useState<Pending | null>(null);
  const [writing, setWriting] = useState(false);
  /** Approvals this session made, with the receipt's `selfApproved`: the wire
   *  names no approver, so only this session can say "by you". */
  const [approvedByMe, setApprovedByMe] = useState<Record<string, boolean>>({});
  const [toast, setToast] = useState<ToastMsg | null>(null);
  const toastTimer = useRef(0);
  /** Bumped when a write settles: focus moves to the heading after that commit,
   *  as `Members.tsx` moves it once its drawer closes. The trigger may be gone
   *  (an approved row has no Approve; an archived row leaves the tab). */
  const [focusHeading, setFocusHeading] = useState(0);
  const headingRef = useRef<HTMLHeadingElement | null>(null);
  useEffect(() => {
    // Effects run after the dialog's own unmount cleanup, so this wins over
    // its return of focus to a trigger that may be about to go.
    if (focusHeading > 0) headingRef.current?.focus();
  }, [focusHeading]);
  /** The screen asks the shared probe again once per session after a 404 list. */
  const probeAsked = useRef(false);
  const goneRef = useRef(false);
  goneRef.current = gone;

  const say = useCallback((kind: ToastMsg["kind"], text: string) => {
    setToast({ kind, text });
    window.clearTimeout(toastTimer.current);
    toastTimer.current = window.setTimeout(() => setToast(null), 4200);
  }, []);
  useEffect(() => () => window.clearTimeout(toastTimer.current), []);

  useEffect(() => onPrivateSessionReset(() => {
    probeAsked.current = false;
    setRows(null);
    setGone(false);
    setConfirming(null);
    setApprovedByMe({});
    setToast(null);
  }), []);

  // A file opened here binds the canvas to this screen's conversation; leaving
  // the screen closes it rather than leave it over another page. A paused
  // session leaves it alone: the session reset decides what it keeps.
  useEffect(() => () => {
    if (privateReadsAllowed() && canvasSnapshot().conversation === DOCUMENTS_CONVERSATION) resetCanvas();
  }, []);

  // A probe that answers available again (a new answer, not the one the list
  // contradicted) clears the unavailable state and lists again.
  useEffect(() => {
    if (answer?.kind !== "available" || !goneRef.current) return;
    setGone(false);
    setAttempt((count) => count + 1);
  }, [answer]);

  const readSeq = useRef(0);
  const refresh = useCallback(
    async (signal?: AbortSignal) => {
      // A paused session refuses every private read; asking would only fail and
      // read as an error. The screen lists again when it is admitted.
      if (!privateReadsAllowed()) return;
      // A post-write read can land after a newer one; only the latest commits.
      const seq = ++readSeq.current;
      setLoading(true);
      setFailed(false);
      try {
        const result = await list(archived, signal);
        if (signal?.aborted || seq !== readSeq.current) return;
        if (result.ok) setRows({ archived, rows: result.value });
        else if (result.failure.status === 404 && refusalCode(result) === "not_found") {
          setGone(true);
          // Let the shared probe learn it too, so the nav item goes. Once per
          // session: if the probe still says available, its answer lists once
          // more, and a lasting 404 stops there instead of looping.
          if (!probeAsked.current) {
            probeAsked.current = true;
            void documentsProbe.invalidate();
          }
        } else setFailed(true);
      } catch {
        // `list` rejects only for an abort: a superseded read is not a failure.
      } finally {
        if (!signal?.aborted && seq === readSeq.current) setLoading(false);
      }
    },
    [archived],
  );

  useEffect(() => {
    if (!available) return;
    const controller = new AbortController();
    void refresh(controller.signal);
    return () => controller.abort();
  }, [available, refresh, attempt]);

  /** After a refusal the types may have changed too (a new approver, a confirmed row). */
  const rereadAll = useCallback(() => {
    void documentsProbe.refresh();
    void refresh();
  }, [refresh]);

  const run = async (pending: Pending) => {
    const { action, row, version } = pending;
    const id = row.file.id;
    setWriting(true);
    let result: DocumentsResult<{ stage: DocumentStage }>;
    let receipt: { selfApproved: boolean } | null = null;
    try {
      if (action === "approve") {
        const answered = await approve(id, version);
        if (answered.ok) receipt = answered.value;
        result = answered;
      } else result = action === "archive" ? await archive(id) : await restore(id);
    } catch {
      // The client rejects only for an abort (an AbortError once the private
      // session paused), and the server may have applied the change by then:
      // say nothing and read again.
      setWriting(false);
      setConfirming(null);
      setFocusHeading((count) => count + 1);
      rereadAll();
      return;
    }
    setWriting(false);
    setConfirming(null);
    setFocusHeading((count) => count + 1);
    if (result.ok) {
      const next = result.value.stage;
      // An archived row leaves the Active tab, a restored one the Archived tab.
      const leaves = action !== "approve" && (next === "archived") !== archived;
      setRows((current) =>
        current && current.archived === archived
          ? {
              archived,
              rows: leaves ? current.rows.filter((entry) => entry.file.id !== id) : current.rows.map((entry) =>
                entry.file.id !== id
                  ? entry
                  : {
                      ...entry,
                      document: {
                        ...entry.document,
                        stage: next,
                        ...(action === "approve" && receipt
                          ? { approvedVersion: version, selfApproved: receipt.selfApproved }
                          : {}),
                      },
                    },
              ),
            }
          : current,
      );
      if (action === "approve" && receipt) {
        const self = receipt.selfApproved;
        setApprovedByMe((current) => ({ ...current, [id]: self }));
      }
      say("ok", action === "approve" ? t("Document approved.") : action === "archive" ? t("Document archived.") : t("Document restored."));
      void refresh();
      return;
    }
    const code = refusalCode(result);
    say("err", action === "approve" ? approveFailure(code, t) : action === "archive" ? archiveFailure(code, t) : restoreFailure(code, t));
    if (action === "approve") rereadAll();
    else void refresh();
  };

  const unavailable = !hosted || answer?.kind === "unavailable" || gone;
  if (unavailable) {
    return (
      <section className="documents-page px-4 lg:px-8 py-5">
        <PageState title={t("Documents aren’t available here")} actions={<Button href={hrefFor({ screen: "home" })}>{t("Home")}</Button>}>
          {t("This workspace doesn’t have document review turned on.")}
        </PageState>
      </section>
    );
  }
  // A probe that failed (and never answered available) is not a capability
  // answer: the screen is unavailable for now, with a retry of the real probe.
  if (answer?.kind === "error" && !probeState?.inFlight) {
    return (
      <section className="documents-page px-4 lg:px-8 py-5">
        <PageState
          title={t("Documents aren’t available here")}
          actions={<Button onClick={() => void documentsProbe.refresh()}>{t("Retry")}</Button>}
        >
          {t("Couldn’t check whether documents are available.")}
        </PageState>
      </section>
    );
  }

  const dateOf = (ms: number | null) =>
    ms === null ? "—" : formatDate(new Date(ms), { year: "numeric", month: "short", day: "numeric" });

  /** The version a row opens: the one under review for its approver, so they
   *  read what they approve; otherwise the newest version this viewer can see. */
  const versionShown = (row: DocumentRow) =>
    row.document.submittedVersion !== null && canApprove(viewer, row.document, typeFor(types, row.document))
      ? row.document.submittedVersion
      : row.file.version;

  const open = (row: DocumentRow) => {
    const { file } = row;
    if (!isArtifactKind(file.kind)) return;
    const version = versionShown(row);
    // The list carries the size and digest of `file.version` only. Another
    // version opens without them, so the drawer never shows one version's
    // digest beside another.
    const ref: ArtifactRef = {
      id: file.id,
      version,
      kind: file.kind,
      title: file.title,
      ...(version === file.version ? { bytes: file.bytes, digest: file.digest } : {}),
    };
    openRecordedArtifact(DOCUMENTS_CONVERSATION, null, ref);
  };

  const ask = (action: Write, row: DocumentRow) => {
    const version = action === "approve" ? row.document.submittedVersion ?? row.file.version : row.file.version;
    setConfirming({ action, row, version });
  };

  const columns: DataTableColumn<DocumentRow>[] = [
    {
      key: "title",
      header: t("Document"),
      cell: (row) =>
        isArtifactKind(row.file.kind) ? (
          <button
            type="button"
            className="text-left text-ink-100 hover:text-accent hover:underline focus-visible:outline-2 focus-visible:outline-accent"
            onClick={() => open(row)}
          >
            {row.file.title}
          </button>
        ) : (
          <span className="text-ink-100">{row.file.title}</span>
        ),
    },
    { key: "type", header: t("Document type"), cell: (row) => row.document.type },
    {
      key: "stage",
      header: t("Stage"),
      cell: (row) => {
        const mine = approvedByMe[row.file.id];
        return (
          <span className="inline-flex flex-wrap items-center gap-1.5">
            <span className={STAGE_CHIP[row.document.stage]}>{t(STAGE_LABEL[row.document.stage])}</span>
            {row.document.stage === "approved" && mine !== undefined && (
              <span className="text-label text-ink-400">{mine ? t("Approved by you (self)") : t("Approved by you")}</span>
            )}
          </span>
        );
      },
    },
    {
      key: "review",
      header: t("Version under review"),
      cell: (row) =>
        row.document.stage === "in_review" && row.document.submittedVersion !== null ? `v${row.document.submittedVersion}` : "—",
      cellClassName: "num",
    },
    { key: "delivered", header: t("Delivered"), cell: (row) => dateOf(row.document.deliveredAt), cellClassName: "num" },
    ...(archived
      ? [{ key: "archived", header: t("Archived"), cell: (row: DocumentRow) => dateOf(row.document.archivedAt), cellClassName: "num" }]
      : []),
    {
      key: "actions",
      header: t("Actions"),
      cell: (row) => {
        const type = typeFor(types, row.document);
        const title = row.file.title;
        return (
          <span className="inline-flex flex-wrap gap-1.5">
            {row.document.submittedVersion !== null && canApprove(viewer, row.document, type) && (
              <Button size="sm" variant="primary" aria-label={t("Approve {title}").replace("{title}", () => title)} onClick={() => ask("approve", row)}>
                {t("Approve")}
              </Button>
            )}
            {canArchive(viewer, row.document) && (
              <Button size="sm" aria-label={t("Archive {title}").replace("{title}", () => title)} onClick={() => ask("archive", row)}>
                {t("Archive")}
              </Button>
            )}
            {canRestore(viewer, row.document) && (
              <Button size="sm" aria-label={t("Restore {title}").replace("{title}", () => title)} onClick={() => ask("restore", row)}>
                {t("Restore")}
              </Button>
            )}
          </span>
        );
      },
    },
  ];

  const tabRows = rows !== null && rows.archived === archived ? rows.rows : null;
  const shown = tabRows === null ? null : !archived && stage ? tabRows.filter((row) => row.document.stage === stage) : tabRows;

  let body;
  if (answer === null || answer.kind === "error") {
    // Still checking, or retrying a failed probe.
    body = <Loading>{t("Checking documents…")}</Loading>;
  } else if (shown === null) {
    body = failed && !loading ? (
      <ErrorNotice onRetry={() => setAttempt((count) => count + 1)} retryLabel={t("Retry")}>
        {t("Couldn’t load documents.")}
      </ErrorNotice>
    ) : (
      <Loading>{t("Loading documents…")}</Loading>
    );
  } else if (tabRows!.length === 0) {
    // The server lists the most recent files only (ungoverned ones count toward
    // its limit), so an empty Active tab does not claim there are none at all.
    body = archived ? (
      <EmptyState name="documents" title={t("Nothing archived")} />
    ) : (
      <EmptyState name="documents" title={t("No documents to show")}>
        {t("Only the most recent files are listed, so older documents may not appear here.")}
      </EmptyState>
    );
  } else if (shown.length === 0) {
    body = (
      <EmptyState name="documents-filtered" title={t("No documents at this stage")}>
        <Link href={hrefFor({ screen: "documents", archived: false })} replace className="text-accent hover:underline">
          {t("Show all stages")}
        </Link>
      </EmptyState>
    );
  } else {
    body = (
      <DataTable
        columns={columns}
        rows={shown}
        rowKey={(row) => row.file.id}
        label={archived ? t("Archived documents") : t("Active documents")}
        tableClassName="documents-table"
        rowProps={(row) => ({ "data-document-id": row.file.id })}
      />
    );
  }

  const confirmingTitle = confirming?.row.file.title ?? "";
  return (
    <section className="documents-page px-4 lg:px-8 py-5 grid gap-4" aria-labelledby="documents-title" aria-busy={loading || undefined}>
      <h1 id="documents-title" ref={headingRef} tabIndex={-1} className="text-section font-semibold text-ink-100 focus:outline-none">
        {t("Documents")}
      </h1>
      <SectionTabs
        label={t("Documents")}
        bare
        tabs={[
          { label: "Active", href: hrefFor({ screen: "documents", archived: false }), on: !archived },
          { label: "Archived", href: hrefFor({ screen: "documents", archived: true }), on: archived },
        ]}
      />
      {!archived && (
        <nav aria-label={t("Filter by stage")} data-stage-filter className="flex flex-wrap items-center gap-1.5 min-w-0">
          {STAGE_FILTERS.map((filter) => {
            const on = (stage ?? null) === filter.stage;
            return (
              <Link
                key={filter.label}
                href={hrefFor({ screen: "documents", archived: false, ...(filter.stage ? { stage: filter.stage } : {}) })}
                replace
                aria-current={on ? "page" : undefined}
                className={`chip ${on ? "bg-accent/15 text-accent" : "bg-ink-800 text-ink-400 hover:text-ink-100"}`}
              >
                {t(filter.label)}
              </Link>
            );
          })}
        </nav>
      )}
      {body}
      {failed && shown !== null && (
        <ErrorNotice onRetry={() => setAttempt((count) => count + 1)} retryLabel={t("Retry")}>
          {t("Couldn’t refresh this list, so it may be out of date.")}
        </ErrorNotice>
      )}
      {confirming && (
        <ConfirmDialog
          title={
            confirming.action === "approve"
              ? t("Approve document?")
              : confirming.action === "archive"
                ? t("Archive document?")
                : t("Restore document?")
          }
          body={
            <p>
              {confirming.action === "approve"
                ? t("Approve version {version} of “{title}”? A later version needs its own approval.")
                    .replace("{version}", () => String(confirming.version))
                    .replace("{title}", () => confirmingTitle)
                : confirming.action === "archive"
                  ? t("Archive “{title}”? It moves to the Archived tab, and the owner can restore it.").replace("{title}", () => confirmingTitle)
                  : t("Restore “{title}”? It returns to the stage it had before it was archived. Nothing is re-run.").replace("{title}", () => confirmingTitle)}
            </p>
          }
          confirmLabel={confirming.action === "approve" ? t("Approve") : confirming.action === "archive" ? t("Archive") : t("Restore")}
          confirmVariant={confirming.action === "archive" ? "danger" : "primary"}
          pending={writing}
          error={null}
          onCancel={() => {
            if (!writing) setConfirming(null);
          }}
          onConfirm={() => void run(confirming)}
        />
      )}
      <Toast msg={toast} />
    </section>
  );
}
