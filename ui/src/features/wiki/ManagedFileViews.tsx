import { useEffect, useRef, useState } from "react";
import { useTranslate } from "../../lib/locale";
import { onPrivateReadsPaused } from "../../lib/privateSession";
import type { Route } from "../../lib/router";
import Button from "../../ui/Button";
import Link from "../../ui/Link";
import ConfirmDialog from "../app-shell/shared/ConfirmDialog";
import ArtifactView, { downloadName, saveBytes, useArtifactBytes } from "../canvas/ArtifactView";
import { ARTIFACT_KINDS, type ArtifactKind } from "../canvas/canvasStore";
import { approve, archive, restore } from "../documents/documentsClient";
import { getManagedFile, listManagedFiles, type WikiManagedFile } from "./managedFiles";

export function managedRoute(row: WikiManagedFile): Route {
  return { screen: "wiki", mode: "browse", path: null, query: null, file: row.file.id, version: row.file.version };
}

export function ManagedList({ path, archived, query, issue, navHref }: { path?: string; archived?: boolean; query?: string; issue?: string; navHref: (route: Route) => string }) {
  const t = useTranslate();
  const [rows, setRows] = useState<WikiManagedFile[] | null>(null);
  const [error, setError] = useState<string | null>(null);
  useEffect(() => {
    const controller = new AbortController();
    setRows(null); setError(null);
    void listManagedFiles(issue !== undefined ? { issue } : { path: path ?? "", archived, q: query }, controller.signal).then(setRows).catch((e) => { if (!controller.signal.aborted) setError(e instanceof Error ? e.message : String(e)); });
    return () => controller.abort();
  }, [path, archived, query, issue]);
  useEffect(() => onPrivateReadsPaused(() => { setRows(null); setError(null); }), []);
  if (error) return <p role="alert" className="text-fail">{error}</p>;
  if (!rows) return <p role="status" className="text-label text-ink-500">{t("Loading files…")}</p>;
  if (!rows.length) return archived ? <p className="text-secondary text-ink-400">{t("No archived files.")}</p> : null;
  return (
    <section className="card min-w-0 p-3">
      <h2 className="text-cardtitle font-semibold text-ink-100">{t(issue !== undefined ? "Linked files" : "Workspace files")}</h2>
      <ul className="list-none m-0 p-0">
        {rows.map((row) => (
          <li key={`${row.file.id}:${row.file.version}`} className="border-b border-ink-800 py-2">
            <Link className="text-accent hover:underline" href={navHref(managedRoute(row))}>{row.file.title} · v{row.file.version}</Link>
          </li>
        ))}
      </ul>
    </section>
  );
}

type FileAction = "approve" | "archive" | "restore";

function FileContent({ row, readOnly, choose }: { row: WikiManagedFile; readOnly: boolean; choose: (action: FileAction) => void }) {
  const t = useTranslate();
  const { id, version, title } = row.file;
  const kind = (ARTIFACT_KINDS as readonly string[]).includes(row.file.kind) ? row.file.kind as ArtifactKind : null;
  const content = useArtifactBytes("wiki:managed", id, version);
  const ref = useRef<HTMLDivElement>(null);
  const data = content.status === "ready" ? content.data : null;
  return (
    <>
      <h1 className="text-section font-semibold text-ink-100">{title}</h1>
      <p className="text-secondary text-ink-400">
        {t("Version")} {version}{row.document ? ` · ${t(row.document.stage.replaceAll("_", " "))} · ${row.document.type}` : ""}
      </p>
      <div className="flex flex-wrap gap-2">
        {data && kind && <Button onClick={() => saveBytes(data.bytes, data.mime, downloadName(title, kind, data.mime))}>{t("Download")}</Button>}
        {!readOnly && row.actions.approve && row.document?.submittedVersion === version && (
          <Button variant="primary" disabled={!data} onClick={() => choose("approve")}>{t("Approve")} v{version}</Button>
        )}
        {!readOnly && row.actions.archive && <Button onClick={() => choose("archive")}>{t("Archive")}</Button>}
        {!readOnly && row.actions.restore && <Button onClick={() => choose("restore")}>{t("Restore")}</Button>}
      </div>
      {content.status === "loading" && <p role="status">{t("Loading file…")}</p>}
      {content.status === "failed" && <p role="alert" className="text-fail">{content.error}</p>}
      {data && kind && (
        <div className="card min-w-0 overflow-auto p-3">
          <ArtifactView kind={kind} title={title} data={data} contentRef={ref} />
        </div>
      )}
    </>
  );
}

export function ManagedFilePane({ id, version, readOnly, navHref, onChange }: { id: string; version: number; readOnly: boolean; navHref: (route: Route) => string; onChange?: () => void }) {
  const t = useTranslate();
  const [row, setRow] = useState<WikiManagedFile | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [attempt, setAttempt] = useState(0);
  const [action, setAction] = useState<FileAction | null>(null);
  const [actionError, setActionError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  useEffect(() => {
    const controller = new AbortController();
    setRow(null); setError(null);
    void getManagedFile(id, version, controller.signal).then(setRow).catch((e) => { if (!controller.signal.aborted) setError(e instanceof Error ? e.message : String(e)); });
    return () => controller.abort();
  }, [id, version, attempt]);
  useEffect(() => onPrivateReadsPaused(() => { setRow(null); setError(null); setAction(null); }), []);
  const run = async () => {
    if (!action || !row || readOnly || busy) return;
    setBusy(true);
    setActionError(null);
    try {
      const result = action === "approve" ? await approve(id, version) : action === "archive" ? await archive(id) : await restore(id);
      if (!result.ok) {
        setActionError(result.failure.message);
        return;
      }
      setAction(null);
      setAttempt((n) => n + 1);
      onChange?.();
    } catch {
      setActionError(t("Could not confirm the document change. Close and reload the file before retrying."));
    } finally {
      setBusy(false);
    }
  };
  const label = t(action === "archive" ? "Archive" : action === "restore" ? "Restore" : "Approve");
  return (
    <section className="grid min-w-0 gap-4 p-4">
      <Link className="text-accent hover:underline" href={navHref({ screen: "wiki", mode: "browse", path: null, query: null })}>← {t("Files")}</Link>
      {error && (
        <p role="alert" className="text-fail">
          {error} <button className="lnk" type="button" onClick={() => setAttempt((n) => n + 1)}>{t("Retry")}</button>
        </p>
      )}
      {!row && !error && <p role="status">{t("Loading file…")}</p>}
      {row && <FileContent row={row} readOnly={readOnly} choose={(next) => { setActionError(null); setAction(next); }} />}
      {action && row && <ConfirmDialog
        title={t(action === "approve" ? "Approve document?" : action === "archive" ? "Archive document?" : "Restore document?")}
        body={<p>{action === "approve"
          ? t("Approve version {version} of “{title}”? A later version needs its own approval.").replace("{version}", `${version}`).replace("{title}", () => row.file.title)
          : row.file.title}</p>}
        confirmLabel={label}
        confirmVariant={action === "archive" ? "danger" : "primary"}
        pending={busy}
        error={actionError}
        onCancel={() => { if (!busy) setAction(null); }}
        onConfirm={() => void run()}
      />}
    </section>
  );
}
