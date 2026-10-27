import { useEffect, useRef, useState } from "react";
import type { Route } from "../../lib/router";
import { fmtBytes } from "../../lib/fmt";
import { IconCheck, IconUpload, IconWarning } from "../../ui/icons";
import { clearTransferResume, importFile, uploadMultipart, readResume, forgetResume, type ImportResult, type ResumeEntry } from "./transfer";
import {
  capLabel,
  counts,
  pending,
  queued,
  releaseUpload,
  serverErrorText,
  startUpload,
  uploadDone,
  uploadFailed,
  uploadProgress,
  type UploadItem,
} from "./upload";
import Button from "../../ui/Button";
import { KindIcon, Note, WikiToolbar } from "./shared";
import { useTranslate } from "../../lib/locale";
import { beginPrivateOperation, onPrivateReadsPaused, onPrivateSessionReset, type PrivateOperation } from "../../lib/privateSession";

/**
 * The upload pane (CAD-581): a drag-and-drop overlay plus the multi-file
 * progress list. Oversize files are rejected by the client pre-check and
 * never leave the browser; anything the server refuses lands on that
 * file's row alone.
 */

export default function UploadPane({
  dir,
  navHref,
  readOnly,
  onToast,
  onUploaded,
  multipart = false,
}: {
  dir: string;
  navHref: (route: Route) => string;
  readOnly: boolean;
  onToast: (kind: "ok" | "err" | "warn", text: string) => void;
  onUploaded: () => void;
  /** Hosted boards upload with one multipart POST; local boards use transfer jobs. */
  multipart?: boolean;
}) {
  const [items, setItems] = useState<UploadItem[]>([]);
  const [drag, setDrag] = useState(0);
  const [held, setHeld] = useState(false);
  const [updateExisting, setUpdateExisting] = useState(false);
  const [resumes, setResumes] = useState<ResumeEntry[]>([]);
  const updateRef = useRef(false);
  const itemsRef = useRef<UploadItem[]>([]);
  const files = useRef(new Map<string, File>());
  const nextId = useRef(0);
  const running = useRef(false);
  const heldRef = useRef(false);
  const attempt = useRef(0);
  const operationRef = useRef<PrivateOperation | null>(null);
  const fileInput = useRef<HTMLInputElement>(null);
  updateRef.current = updateExisting;

  const abandon = () => {
    attempt.current += 1;
    running.current = false;
    const operation = operationRef.current;
    operationRef.current = null;
    operation?.dispose();
    const active = itemsRef.current.find((item) => item.state === "uploading");
    const preview = active ? releaseUpload(itemsRef.current, active.id) : itemsRef.current;
    setHeld(pending(preview).length > 0);
    setItems((current) => {
      const uploading = current.find((item) => item.state === "uploading");
      const next = uploading ? releaseUpload(current, uploading.id) : current;
      const waiting = pending(next).length > 0;
      heldRef.current = waiting;
      itemsRef.current = next;
      return next;
    });
  };

  const add = (list: FileList | null) => {
    if (readOnly || !list || list.length === 0) return;
    const picked = Array.from(list);
    const fresh = queued(
      picked.map((f) => ({ name: f.name, size: f.size })),
      nextId.current,
    );
    nextId.current += fresh.length;
    picked.forEach((file, i) => files.current.set(fresh[i].id, file));
    const hadPending = pending(itemsRef.current).length > 0;
    const next = [...itemsRef.current, ...fresh];
    itemsRef.current = next;
    if (!hadPending) {
      heldRef.current = false;
      setHeld(false);
    }
    setItems(next);
  };

  // An import in flight records itself as soon as `begin` answers, so reading
  // here would present the running transfer as an interrupted one. Refresh
  // only while nothing is running: every completion path sets `items` after
  // it clears the flag, which re-runs this effect.
  useEffect(() => {
    if (running.current) return;
    setResumes(readResume(dir));
  }, [dir, items]);

  useEffect(() => {
    const stopPause = onPrivateReadsPaused(abandon);
    const stopReset = onPrivateSessionReset(() => {
      attempt.current += 1;
      running.current = false;
      heldRef.current = false;
      const operation = operationRef.current;
      operationRef.current = null;
      operation?.dispose();
      files.current.clear();
      itemsRef.current = [];
      clearTransferResume();
      setResumes([]);
      setHeld(false);
      setItems([]);
    });
    return () => {
      stopPause();
      stopReset();
    };
  }, []);

  useEffect(() => () => {
    attempt.current += 1;
    running.current = false;
    const operation = operationRef.current;
    operationRef.current = null;
    operation?.dispose();
  }, []);

  useEffect(() => {
    if (readOnly || heldRef.current || running.current) return;
    const id = pending(items)[0];
    if (!id) return;
    const file = files.current.get(id);
    if (!file) {
      setItems((current) => {
        const next = uploadFailed(current, id, "the file is no longer available — add it again");
        itemsRef.current = next;
        return next;
      });
      return;
    }
    let operation: PrivateOperation;
    try {
      operation = beginPrivateOperation();
    } catch {
      return;
    }
    const epoch = attempt.current;
    operationRef.current = operation;
    running.current = true;
    setItems((current) => {
      const next = startUpload(current, id);
      itemsRef.current = next;
      return next;
    });
    const currentAttempt = () => epoch === attempt.current && operationRef.current === operation && !operation.signal.aborted;
    const releaseTransport = () => {
      if (operationRef.current === operation) operationRef.current = null;
      running.current = false;
    };
    (multipart ? uploadMultipart : importFile)(dir, file, updateRef.current, (percent) => {
      if (!currentAttempt()) return;
      setItems((current) => {
        const next = uploadProgress(current, id, percent);
        itemsRef.current = next;
        return next;
      });
    }, operation)
      .then((result: ImportResult) => {
        if (!currentAttempt()) return;
        const abandonedTransfer = operation.signal.aborted || heldRef.current;
        releaseTransport();
        const detail = `${result.outcome} · extraction ${result.extraction} · index ${result.index}`;
        setItems((current) => {
          const next = uploadDone(current, id).map((item) => item.id === id ? { ...item, detail } : item);
          itemsRef.current = next;
          return next;
        });
        if (abandonedTransfer) return;
        const blocked = result.index !== "covered" || result.extraction === "failed" || result.extraction === "empty" || result.extraction === "unsupported" || result.extraction === "conflict";
        if (blocked) onToast("warn", `${file.name} was saved; extraction ${result.extraction}; index ${result.index}${result.error ? `: ${result.error}` : ""}`);
        onUploaded();
      })
      .catch((error: unknown) => {
        if (!currentAttempt()) return;
        if (error instanceof DOMException && error.name === "AbortError") return;
        const abandonedTransfer = operation.signal.aborted || heldRef.current;
        releaseTransport();
        setItems((current) => {
          const next = uploadFailed(current, id, serverErrorText(error));
          itemsRef.current = next;
          return next;
        });
        if (!abandonedTransfer) onUploaded();
      })
      .finally(() => {
        operation.dispose();
      });
  }, [items, dir, onUploaded, onToast, readOnly, held]);

  const tally = counts(items);
  const active = items.some((item) => item.state === "queued" || item.state === "uploading");
  const browseHref = navHref({ screen: "wiki", mode: "browse", path: dir || null, query: null });
  const t = useTranslate();

  return (
    <>
      <WikiToolbar path={dir} navHref={navHref} label={t("Upload")} actions={<>
          <Button href={browseHref}>{t("Done")}</Button>
      </>} />

      {readOnly && (
        <Note warn>{t("writes are disabled on this board — sign in as the operator to upload.")}</Note>
      )}

      <div
        className="wk-dropwrap"
        onDragEnter={(e) => {
          e.preventDefault();
          if (!readOnly) setDrag((n) => n + 1);
        }}
        onDragOver={(e) => e.preventDefault()}
        onDragLeave={(e) => {
          e.preventDefault();
          setDrag((n) => Math.max(0, n - 1));
        }}
        onDrop={(e) => {
          e.preventDefault();
          setDrag(0);
          add(e.dataTransfer?.files ?? null);
        }}
      >
        <div className="wk-drop">
          <IconUpload />
          <div className="wk-etitle">{t("Drop files to upload")}</div>
          <div className="kicker">
            {t("Up to")} {capLabel()} {t("per file")}
          </div>
          <label className="kicker">
            <input type="checkbox" checked={updateExisting} disabled={readOnly} onChange={(event) => setUpdateExisting(event.target.checked)} /> {t("Update changed files")}
          </label>
          <Button disabled={readOnly} onClick={() => fileInput.current?.click()}>{t("Choose files")}</Button>
          <input
            ref={fileInput}
            aria-label={t("Files to upload")}
            type="file"
            multiple
            className="wk-fileinput"
            disabled={readOnly}
            onChange={(e) => {
              add(e.target.files);
              e.target.value = "";
            }}
          />
        </div>
        {drag > 0 && !readOnly && (
          <div className="wk-ovr">
            <div className="wk-ovrbox">
              <div className="wk-etitle">{t("Drop files to upload")}</div>
              <div className="kicker">{t("release to queue them")}</div>
            </div>
          </div>
        )}
      </div>

      {resumes.length > 0 && (
        <Note>
          {t("An import was interrupted. Reselect the same file to continue that job.")}
          <ul>{resumes.map((entry) => <li key={entry.job_id}>{entry.path} · {entry.job_id}</li>)}</ul>
          <Button onClick={() => { forgetResume(dir); setResumes([]); }}>{t("Forget this interrupted import on this browser")}</Button>
        </Note>
      )}

      {items.length > 0 && (
        <div className="wk-uplist">
          <div className="slabel" role="status">
            {active ? `${tally.done} of ${tally.total} uploaded` : `${tally.done} uploaded`}
            {tally.failed > 0 ? ` · ${tally.failed} failed` : ""}
          </div>
          {items.map((item) => (
            <div key={item.id} className={`wk-uprow ${item.state}`}>
              <KindIcon kind="file" />
              <span className="wk-uname">{item.name}</span>
              <span className="wk-usize num">{fmtBytes(item.size)}</span>
              <span className="wk-ustate">
                {item.state === "done" ? (
                  <>
                    <IconCheck />
                    done
                  </>
                ) : item.state === "error" ? (
                  <>
                    <IconWarning size={13} />
                    {item.error ?? "rejected"}
                  </>
                ) : item.state === "uploading" ? (
                  <>
                    <span className="wk-upbar">
                      <i style={{ width: `${item.progress}%` }} />
                    </span>
                    <span className="num">{item.progress}%</span>
                  </>
                ) : (
                  <span className="kicker">queued</span>
                )}
                {item.detail && <span className="kicker">{item.detail}</span>}
              </span>
            </div>
          ))}
          <div className="wk-tools">
            {held && pending(items).length > 0 && (
              <Button onClick={() => {
                heldRef.current = false;
                setHeld(false);
              }}>{t("Retry uploads")}</Button>
            )}
            <Button
              disabled={active}
              onClick={() => {
                onToast("ok", `${tally.done} uploaded${tally.failed ? `, ${tally.failed} failed` : ""}`);
                itemsRef.current = [];
                setItems([]);
                files.current.clear();
              }}
            >
              {t("Clear completed uploads")}
            </Button>
          </div>
        </div>
      )}
    </>
  );
}
