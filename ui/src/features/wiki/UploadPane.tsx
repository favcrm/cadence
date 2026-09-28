import { useEffect, useRef, useState } from "react";
import type { Route } from "../../lib/router";
import { fmtBytes } from "../../lib/fmt";
import { IconCheck, IconUpload, IconWarning } from "../../ui/icons";
import { uploadFile } from "./api";
import {
  capLabel,
  counts,
  pending,
  queued,
  serverErrorText,
  startUpload,
  uploadDone,
  uploadFailed,
  uploadProgress,
  type UploadItem,
} from "./upload";
import Button from "../../ui/Button";
import { KindIcon, Note, WikiToolbar } from "./shared";

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
}: {
  dir: string;
  navHref: (route: Route) => string;
  readOnly: boolean;
  onToast: (kind: "ok" | "err" | "warn", text: string) => void;
  onUploaded: () => void;
}) {
  const [items, setItems] = useState<UploadItem[]>([]);
  const [drag, setDrag] = useState(0);
  const files = useRef(new Map<string, File>());
  const nextId = useRef(0);
  const running = useRef(false);
  const fileInput = useRef<HTMLInputElement>(null);

  const add = (list: FileList | null) => {
    if (readOnly || !list || list.length === 0) return;
    const picked = Array.from(list);
    const fresh = queued(
      picked.map((f) => ({ name: f.name, size: f.size })),
      nextId.current,
    );
    nextId.current += fresh.length;
    picked.forEach((file, i) => files.current.set(fresh[i].id, file));
    setItems((current) => [...current, ...fresh]);
  };

  useEffect(() => {
    const id = pending(items)[0];
    if (readOnly || !id || running.current) return;
    const file = files.current.get(id);
    if (!file) {
      setItems((current) => uploadFailed(current, id, "the file is no longer available — add it again"));
      return;
    }
    running.current = true;
    setItems((current) => startUpload(current, id));
    uploadFile(dir, file, (percent) => setItems((current) => uploadProgress(current, id, percent)))
      .then((result) => {
        setItems((current) => uploadDone(current, id));
        if (result?.extraction && result.extraction.status !== "indexed") {
          onToast("warn", `${file.name} was saved; text extraction ${result.extraction.status}: ${result.extraction.reason ?? "review the source"}`);
        }
      })
      .catch((error) => setItems((current) => uploadFailed(current, id, serverErrorText(error))))
      .finally(() => {
        running.current = false;
        onUploaded();
      });
  }, [items, dir, onUploaded, onToast, readOnly]);

  const tally = counts(items);
  const active = items.some((item) => item.state === "queued" || item.state === "uploading");
  const browseHref = navHref({ screen: "wiki", mode: "browse", path: dir || null, query: null });

  return (
    <>
      <WikiToolbar path={dir} navHref={navHref} label="Upload" actions={<>
          <Button href={browseHref}>Done</Button>
      </>} />

      {readOnly && (
        <Note warn>writes are disabled on this board — sign in as the operator to upload.</Note>
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
          <div className="wk-etitle">Drop files to upload</div>
          <div className="kicker">
            Up to {capLabel()} per file
          </div>
          <Button disabled={readOnly} onClick={() => fileInput.current?.click()}>Choose files</Button>
          <input
            ref={fileInput}
            aria-label="Files to upload"
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
              <div className="wk-etitle">Drop files to upload</div>
              <div className="kicker">release to queue them</div>
            </div>
          </div>
        )}
      </div>

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
              </span>
            </div>
          ))}
          <div className="wk-tools">
            <Button
              disabled={active}
              onClick={() => {
                onToast("ok", `${tally.done} uploaded${tally.failed ? `, ${tally.failed} failed` : ""}`);
                setItems([]);
                files.current.clear();
              }}
            >
              Clear completed uploads
            </Button>
          </div>
        </div>
      )}
    </>
  );
}
