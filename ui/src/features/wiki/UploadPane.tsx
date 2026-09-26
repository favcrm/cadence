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
import { Crumbs, KindIcon, Note } from "./shared";

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

  const add = (list: FileList | null) => {
    if (!list || list.length === 0) return;
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
    if (!id || running.current) return;
    const file = files.current.get(id);
    if (!file) {
      setItems((current) => uploadFailed(current, id, "the file is no longer available — add it again"));
      return;
    }
    running.current = true;
    setItems((current) => startUpload(current, id));
    uploadFile(dir, file, (percent) => setItems((current) => uploadProgress(current, id, percent)))
      .then(() => setItems((current) => uploadDone(current, id)))
      .catch((error) => setItems((current) => uploadFailed(current, id, serverErrorText(error))))
      .finally(() => {
        running.current = false;
        onUploaded();
      });
  }, [items, dir, onUploaded]);

  const tally = counts(items);
  const browseHref = navHref({ screen: "wiki", mode: "browse", path: dir || null, query: null });

  return (
    <>
      <div className="wk-bar">
        <Crumbs
          path={dir}
          hrefFor={(p) => navHref({ screen: "wiki", mode: "browse", path: p || null, query: null })}
        />
        <div className="wk-tools">
          <Button href={browseHref}>Done</Button>
        </div>
      </div>

      {readOnly ? (
        <Note warn>writes are disabled on this board — sign in as the operator to upload.</Note>
      ) : (
        <Note>
          uploads land in <span className="num">{dir ? `${dir}/` : "~/pm/wiki"}</span> · {capLabel()} per file
        </Note>
      )}

      <div
        className="wk-dropwrap"
        onDragEnter={(e) => {
          e.preventDefault();
          setDrag((n) => n + 1);
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
            into {dir ? `${dir}/` : "~/pm/wiki"} · {capLabel()} per file
          </div>
          <label className="btn">
            choose files
            <input
              type="file"
              multiple
              className="wk-fileinput"
              disabled={readOnly}
              onChange={(e) => {
                add(e.target.files);
                e.target.value = "";
              }}
            />
          </label>
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
          <div className="slabel">
            uploads · {items.length}
            {tally.failed > 0 ? ` · ${tally.failed} failed` : tally.done === tally.total ? " · all done" : ""}
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
              onClick={() => {
                onToast("ok", `${tally.done} uploaded${tally.failed ? `, ${tally.failed} failed` : ""}`);
                setItems([]);
                files.current.clear();
              }}
            >
              clear the list
            </Button>
          </div>
        </div>
      )}
    </>
  );
}
