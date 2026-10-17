import { useEffect, useId, useState } from "react";
import { api } from "../../lib/api";
import type { Project } from "../../lib/types";
import type { Route } from "../../lib/router";
import { useTranslate } from "../../lib/locale";
import { beginPrivateOperation, onPrivateSessionReset } from "../../lib/privateSession";
import Button from "../../ui/Button";
import Link from "../../ui/Link";
import { Failure, Loading, Note } from "./shared";
import { exportOriginal } from "./transfer";
import {
  readCollection,
  readCorrections,
  reviewCorrection,
  saveCollection,
  type CollectionMetadata,
  type CollectionState,
  type DocumentMetadata,
  type KnowledgeCorrection,
  type KnowledgeDocument,
} from "./knowledge";

function blank(value: string): string | null {
  const trimmed = value.trim();
  return trimmed ? trimmed : null;
}

function processingLabel(state: KnowledgeDocument["processing"]): string {
  if (state === "covered") return "Search text covered";
  if (state === "pending") return "Processing pending";
  if (state === "empty") return "No extractable text";
  if (state === "failed") return "Extraction failed";
  if (state === "unsupported") return "Original only";
  return "Processing unknown";
}

function approvalLabel(doc: KnowledgeDocument): string {
  if (doc.approval_current === null) return "Currentness unknown";
  if (doc.metadata.approved_revision === null) return "Approval unknown";
  if (doc.approval_current) return "This revision is the approved revision";
  return "Approved revision differs from the current revision";
}

export default function CollectionPane({
  path,
  navHref,
  readOnly,
}: {
  path: string;
  navHref: (route: Route) => string;
  readOnly: boolean;
}) {
  const t = useTranslate();
  const formId = useId();
  const [state, setState] = useState<CollectionState | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [denied, setDenied] = useState(false);
  const [draft, setDraft] = useState<CollectionMetadata | null>(null);
  const [projects, setProjects] = useState<Project[] | null>(null);
  const [projectError, setProjectError] = useState<string | null>(null);
  const [corrections, setCorrections] = useState<KnowledgeCorrection[] | null>(null);
  const [canReview, setCanReview] = useState(false);
  const [correctionError, setCorrectionError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const [notice, setNotice] = useState<string | null>(null);

  const load = () => {
    let operation;
    try {
      operation = beginPrivateOperation();
    } catch (reason) {
      setError(reason instanceof Error ? reason.message : String(reason));
      return;
    }
    setError(null);
    setDenied(false);
    void readCollection(path, operation)
      .then((next) => {
        operation.assertCurrent();
        setState(next);
        setDraft({
          ...next.metadata,
          documents: next.documents.map((doc) => doc.metadata),
        });
      })
      .catch((reason: unknown) => {
        if (operation.signal.aborted) return;
        const status = reason instanceof Error && "status" in reason ? Number((reason as { status: number }).status) : 0;
        if (status === 401 || status === 403) {
          setDenied(true);
          setState(null);
          setDraft(null);
          return;
        }
        setError(reason instanceof Error ? reason.message : String(reason));
      })
      .finally(() => operation.dispose());
    let reviews;
    try {
      reviews = beginPrivateOperation();
    } catch (reason) {
      setCorrectionError(reason instanceof Error ? reason.message : String(reason));
      return;
    }
    void readCorrections(path, reviews)
      .then((next) => {
        reviews.assertCurrent();
        setCorrections(next.requests);
        setCanReview(next.permissions.review);
      })
      .catch((reason: unknown) => {
        if (reviews.signal.aborted) return;
        setCorrectionError(reason instanceof Error ? reason.message : String(reason));
      })
      .finally(() => reviews.dispose());
  };

  useEffect(() => {
    load();
    return onPrivateSessionReset(() => {
      setDraft(null);
      setState(null);
      setCorrections(null);
      setNotice(null);
    });
  }, [path]);

  useEffect(() => {
    if (!state?.permissions.publish) return;
    let operation;
    try {
      operation = beginPrivateOperation();
    } catch (reason) {
      setProjectError(reason instanceof Error ? reason.message : String(reason));
      return;
    }
    void api.projects()
      .then((payload) => {
        operation.assertCurrent();
        setProjects(payload.projects);
        setProjectError(null);
      })
      .catch((reason: unknown) => {
        if (!operation.signal.aborted) setProjectError(reason instanceof Error ? reason.message : String(reason));
      })
      .finally(() => operation.dispose());
  }, [state?.permissions.publish]);

  const save = async () => {
    if (!state || !draft || busy || readOnly || !state.permissions.publish) return;
    setBusy(true);
    setNotice(null);
    try {
      const next = await saveCollection(path, state.rev ?? "none", {
        publisher: blank(draft.publisher ?? ""),
        reviewer: blank(draft.reviewer ?? ""),
        review_project: blank(draft.review_project ?? ""),
        documents: draft.documents.map((doc) => ({
          path: doc.path,
          source_type: blank(doc.source_type ?? ""),
          version: blank(doc.version ?? ""),
          applicability: blank(doc.applicability ?? ""),
          supersedes: blank(doc.supersedes ?? ""),
          approved_revision: blank(doc.approved_revision ?? ""),
        })),
      });
      setState(next);
      setDraft({ ...next.metadata, documents: next.documents.map((doc) => doc.metadata) });
      setNotice(t("Collection responsibilities saved. They do not change who can read or edit."));
    } catch (reason) {
      setNotice(reason instanceof Error ? reason.message : String(reason));
    } finally {
      setBusy(false);
    }
  };

  const download = async (docPath: string, rev: string, name: string) => {
    const operation = beginPrivateOperation();
    try {
      const blob = await exportOriginal(docPath, rev, operation);
      operation.assertCurrent();
      const url = URL.createObjectURL(blob);
      const link = document.createElement("a");
      link.href = url;
      link.download = name;
      link.click();
      window.setTimeout(() => URL.revokeObjectURL(url), 1000);
    } catch (reason) {
      if (!operation.signal.aborted) setNotice(reason instanceof Error ? reason.message : String(reason));
    } finally {
      operation.dispose();
    }
  };

  if (denied) return <Note warn>{t("Access denied. Collection names, originals and corrections stay hidden.")}</Note>;
  if (error) return <Failure what="could not load collection metadata" error={error} onRetry={load} />;
  if (!state || !draft) return <Loading what="loading collection metadata…" />;

  const publish = state.permissions.publish && !readOnly;
  const updateDoc = (index: number, patch: Partial<DocumentMetadata>) => {
    setDraft({ ...draft, documents: draft.documents.map((doc, i) => i === index ? { ...doc, ...patch } : doc) });
  };

  return (
    <section className="wk-collection" aria-label={t("Shared collection")}>
      <div className="wk-collection-meta">
        <p>
          <span className="slabel">{t("Publisher")}</span>{" "}
          {publish ? (
            <input className="field" aria-label={t("Publisher")} value={draft.publisher ?? ""} onChange={(event) => setDraft({ ...draft, publisher: event.target.value })} />
          ) : <strong>{state.metadata.publisher ?? t("Unknown")}</strong>}
        </p>
        <p>
          <span className="slabel">{t("Reviewer")}</span>{" "}
          {publish ? (
            <input className="field" aria-label={t("Reviewer")} value={draft.reviewer ?? ""} onChange={(event) => setDraft({ ...draft, reviewer: event.target.value })} />
          ) : <strong>{state.metadata.reviewer ?? t("Unknown")}</strong>}
        </p>
        <p>
          <label className="slabel" htmlFor={`${formId}-project`}>{t("Review project")}</label>
          {publish ? (
            <select
              id={`${formId}-project`}
              className="field"
              value={draft.review_project ?? ""}
              onChange={(event) => setDraft({ ...draft, review_project: event.target.value })}
            >
              <option value="">{t("Not set")}</option>
              {(projects ?? []).map((project) => <option key={project.key} value={project.key}>{project.key}</option>)}
            </select>
          ) : <strong>{state.metadata.review_project ?? t("Unknown")}</strong>}
          <span className="kicker">{t("This chooses where correction issues are filed. It does not grant access.")}</span>
          {projectError && <span className="text-fail">{projectError}</span>}
        </p>
      </div>
      {(!state.metadata.publisher || !state.metadata.reviewer || !state.metadata.review_project) && (
        <Note>{t("Unknown responsibilities stay unknown. A name here is accountability, not permission.")}</Note>
      )}
      {state.documents.length === 0 ? (
        <Note>{t("No documents are recorded for this collection.")}</Note>
      ) : (
        <div className="wk-collection-table" role="table" aria-label={t("Collection documents")}>
          {state.documents.map((doc) => {
            const approved = doc.metadata.approved_revision;
            return (
              <article key={doc.path} className="wk-collection-row" role="row">
                <div>
                  <div className="wk-uname">{doc.path.split("/").pop()}</div>
                  <div className="kicker">{doc.path}</div>
                  <div className="kicker">{doc.metadata.source_type ?? t("Source type unknown")} · {doc.metadata.version ?? t("Version unknown")}</div>
                  <div className="kicker">{t("Applicability")}: {doc.metadata.applicability ?? t("Unknown")}</div>
                </div>
                <div>
                  <span className={`wk-proc wk-proc-${doc.processing ?? "unknown"}`}>{t(processingLabel(doc.processing))}</span>
                  <div className="kicker">{doc.processing === "covered" ? t("Index covered for this recorded revision") : t("A stored original is not answer-ready")}</div>
                </div>
                <div>
                  <div>{t(approvalLabel(doc))}</div>
                  <div className="kicker">{t("Current revision")}: {doc.rev ?? t("Unknown")}</div>
                  <div className="kicker">{t("Approved revision")}: {approved ?? t("Unknown")}</div>
                </div>
                <div className="wk-tools">
                  {doc.rev ? <Link className="btn" href={navHref({ screen: "wiki", mode: "browse", path: doc.path, query: null, at: doc.rev })}>{t("Open source")}</Link> : <Button disabled>{t("Revision unknown")}</Button>}
                  {doc.rev ? <Button onClick={() => void download(doc.path, doc.rev!, doc.path.split("/").pop() ?? "original")}>{t("Download original")}</Button> : <Button disabled>{t("Revision unknown")}</Button>}
                </div>
              </article>
            );
          })}
        </div>
      )}
      {publish && (
        <form className="wk-collection-docs" onSubmit={(event) => { event.preventDefault(); void save(); }}>
          <h2 className="wk-dialog-title">{t("Document accountability")}</h2>
          {draft.documents.map((doc, index) => (
            <fieldset key={doc.path} className="wk-collection-edit">
              <legend className="kicker">{doc.path}</legend>
              <label>{t("Source type")}<input className="field" value={doc.source_type ?? ""} onChange={(event) => updateDoc(index, { source_type: event.target.value })} /></label>
              <label>{t("Version")}<input className="field" value={doc.version ?? ""} onChange={(event) => updateDoc(index, { version: event.target.value })} /></label>
              <label>{t("Applicability")}<input className="field" value={doc.applicability ?? ""} onChange={(event) => updateDoc(index, { applicability: event.target.value })} /></label>
              <label>{t("Supersedes")}<input className="field" value={doc.supersedes ?? ""} onChange={(event) => updateDoc(index, { supersedes: event.target.value })} /></label>
              <label>{t("Approved revision")}<input className="field" spellCheck={false} value={doc.approved_revision ?? ""} onChange={(event) => updateDoc(index, { approved_revision: event.target.value })} /></label>
              <Button type="button" disabled={!state.documents.find((item) => item.path === doc.path)?.rev} onClick={() => {
                const rev = state.documents.find((item) => item.path === doc.path)?.rev;
                if (rev) updateDoc(index, { approved_revision: rev });
              }}>{t("Use the current revision")}</Button>
            </fieldset>
          ))}
          <Button type="submit" variant="primary" loading={busy}>{t("Save collection")}</Button>
        </form>
      )}
      {notice && <Note>{notice}</Note>}
      <section aria-label={t("Reviewed corrections")}>
        <h2 className="wk-dialog-title">{t("Reviewed corrections")}</h2>
        {correctionError && <Failure what="could not load corrections" error={correctionError} />}
        {corrections === null && !correctionError ? <Loading what="loading corrections…" /> : null}
        {corrections && corrections.length === 0 && <Note>{t("No correction requests")}</Note>}
        {corrections?.map((item) => (
          <CorrectionReview key={item.id} item={item} path={path} canReview={canReview && !readOnly} navHref={navHref} onChanged={load} />
        ))}
      </section>
    </section>
  );
}

function CorrectionReview({
  item,
  path,
  canReview,
  navHref,
  onChanged,
}: {
  item: KnowledgeCorrection;
  path: string;
  canReview: boolean;
  navHref: (route: Route) => string;
  onChanged: () => void;
}) {
  const t = useTranslate();
  const reasonId = useId();
  const [reason, setReason] = useState("");
  const [checked, setChecked] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const stale = item.current !== true;
  const acceptDisabled = busy || stale || !checked || reason.trim() === "" || item.citations.length === 0;
  useEffect(() => {
    const stop = onPrivateSessionReset(() => {
      setReason("");
      setChecked(false);
      setError(null);
    });
    return stop;
  }, []);
  const decide = async (decision: "accept" | "reject") => {
    if (busy || reason.trim() === "") return;
    if (decision === "accept" && acceptDisabled) return;
    setBusy(true);
    setError(null);
    try {
      await reviewCorrection({ path, id: item.id, decision, reason: reason.trim(), source_checked: decision === "accept" && checked });
      setReason("");
      setChecked(false);
      onChanged();
    } catch (reasonError) {
      setError(reasonError instanceof Error ? reasonError.message : String(reasonError));
    } finally {
      setBusy(false);
    }
  };
  return (
    <article className="wk-review card">
      <header className="wk-viewrow">
        <h3>{item.id}</h3>
        <span className={`wk-proc wk-proc-${item.status === "accepted" ? "covered" : item.status === "rejected" ? "failed" : "pending"}`}>{t(item.status)}</span>
      </header>
      <p><span className="slabel">{t("Reported question")}</span> {item.question}</p>
      <p><span className="slabel">{t("Reported answer")}</span> {item.answer}</p>
      <p><span className="slabel">{t("Expected answer")}</span> {item.expected_answer}</p>
      <p><span className="slabel">{t("Member reason")}</span> {item.reason}</p>
      <div className="kicker">{t("Submitted by")} {item.submitted_by.display}</div>
      <ul>
        {item.citations.map((cite) => (
          <li key={`${cite.path}@${cite.revision}`}>
            <Link href={navHref({ screen: "wiki", mode: "browse", path: cite.path, query: null, at: cite.revision, ...(cite.section ? { section: cite.section } : {}), ...(cite.page !== undefined ? { page: cite.page } : {}) })}>
              {cite.path} @ {cite.revision}
            </Link>
          </li>
        ))}
      </ul>
      {item.status !== "pending" && <p>{t("Reviewer reason")}: {item.review_reason ?? t("Unknown")}</p>}
      {item.status === "pending" && canReview && (
        <form onSubmit={(event) => { event.preventDefault(); void decide("reject"); }}>
          {item.current === false && <Note warn>{t("The cited source changed. This request cannot be accepted.")}</Note>}
          {item.current === null && <Note warn>{t("Currentness is unknown. This request cannot be accepted.")}</Note>}
          <label>
            <input type="checkbox" checked={checked} onChange={(event) => setChecked(event.target.checked)} /> {t("I checked the cited sources and applicability.")}
          </label>
          <label htmlFor={reasonId}>{t("Reviewer reason")}</label>
          <textarea id={reasonId} className="field" required rows={3} value={reason} onChange={(event) => setReason(event.target.value)} />
          {error && <p className="text-fail" role="alert">{error}</p>}
          <div className="wk-tools">
            <Button type="button" variant="primary" disabled={acceptDisabled} onClick={() => void decide("accept")}>{t("Accept correction")}</Button>
            <Button type="submit" disabled={busy || reason.trim() === ""}>{t("Reject with reason")}</Button>
          </div>
        </form>
      )}
      {item.status === "pending" && !canReview && <Note>{t("Awaiting an owner review. This report is not answer authority.")}</Note>}
    </article>
  );
}
