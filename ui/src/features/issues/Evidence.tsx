import { useEffect, useState } from "react";
import { api } from "../../lib/api";
import { fmtBytes } from "../../lib/fmt";
import { useTranslate } from "../../lib/locale";
import { beginPrivateOperation, onPrivateReadsPaused, privateFetch, privateReadsAllowed, privateSessionGeneration, type PrivateOperation } from "../../lib/privateSession";
import { sessionHeaders, sessionKey } from "../../lib/sessionKey";
import { Attach } from "./Attach";
import { type IssuePageProps as Props } from "./issuePageProps";

function Artifact({ id, name, image = false }: { id: string; name: string; image?: boolean }) {
  const t = useTranslate();
  const key = sessionKey();
  const generation = privateSessionGeneration();
  const [attempt, setAttempt] = useState(0);
  const [loaded, setLoaded] = useState<{
    id: string; name: string; key: string | null; generation: number; attempt: number;
    url: string; preview: string | null;
  } | null>(null);
  const [failed, setFailed] = useState(false);

  useEffect(() => {
    const abort = new AbortController();
    let operation: PrivateOperation | null = null;
    try {
      operation = beginPrivateOperation(abort.signal);
    } catch {
      return () => abort.abort();
    }
    if (operation === null) return () => abort.abort();
    const session = operation;
    let url: string | null = null;
    let reader: FileReader | null = null;
    const current = () => !abort.signal.aborted && !session.signal.aborted && privateReadsAllowed()
      && privateSessionGeneration() === generation && sessionKey() === key;
    const dispose = () => {
      abort.abort();
      const ownedReader = reader;
      reader = null;
      if (ownedReader) {
        ownedReader.onload = null;
        ownedReader.onerror = null;
        ownedReader.abort();
      }
      const ownedUrl = url;
      url = null;
      if (ownedUrl) URL.revokeObjectURL(ownedUrl);
      setLoaded(null);
      setFailed(false);
    };
    const stop = onPrivateReadsPaused(dispose);
    setLoaded(null);
    setFailed(false);
    privateFetch(api.artifactUrl(id, name), {
      credentials: "same-origin",
      headers: sessionHeaders(),
      signal: abort.signal,
    }, session)
    .then(async (response) => {
        if (!response.ok) throw new Error("Artifact could not be loaded");
        const blob = await response.blob();
        session.assertCurrent();
        if (!current()) return;
        // Only raster images may open inline. Other blobs lose the server's
        // sandbox header, so they must download rather than navigate.
        const raster = /^image\/(?:png|jpeg|gif|webp)$/i.test(blob.type);
        url = URL.createObjectURL(raster ? blob : blob.slice(0, blob.size, "application/octet-stream"));
        const ready = (preview: string | null) => {
          if (current() && url) setLoaded({ id, name, key, generation, attempt, url, preview });
        };
        if (image && raster) {
          // The board CSP allows data: images, but not blob: image sources.
          reader = new FileReader();
          reader.onload = () => {
            if (typeof reader?.result === "string") ready(reader.result);
          };
          reader.onerror = () => { if (current()) setFailed(true); };
          reader.readAsDataURL(blob);
        } else {
          ready(null);
        }
      })
      .catch(() => { if (current()) setFailed(true); });
    return () => {
      stop();
      session.dispose();
      dispose();
    };
  }, [id, name, image, key, generation, attempt]);

  const ready = loaded && loaded.id === id && loaded.name === name
    && loaded.key === key && loaded.generation === generation && loaded.attempt === attempt && privateReadsAllowed()
    ? loaded : null;
  if (ready && !failed) {
    return (
      <a
        className={image ? "block" : "lnk num text-label truncate"}
        href={ready.url}
        download={ready.preview ? undefined : name}
        target={ready.preview ? "_blank" : undefined}
        rel={ready.preview ? "noreferrer" : undefined}
        title={name}
      >
        {ready.preview ? (
          <img src={ready.preview} alt={name} className="w-full h-28 object-cover bg-ink-900" onError={() => setFailed(true)} />
        ) : name}
      </a>
    );
  }
  return (
    <div className={image ? "grid min-h-28 gap-2 p-2.5 text-label" : "min-w-0 text-label"}>
      {!image && <span className="num truncate" title={name}>{name}</span>}
      {failed ? (
        <div role="alert">
          <span>{t("Could not load")} {name}. </span>
          <button type="button" className="lnk" aria-label={`${t("Retry")} ${name}`} onClick={() => setAttempt((value) => value + 1)}>
            {t("Retry")}
          </button>
        </div>
      ) : <span role="status">{t("Loading")} {name}…</span>}
    </div>
  );
}

export function Evidence({
  id,
  images,
  files,
  readOnly,
  onWrite,
  onError,
}: {
  id: string;
  images: { name: string; size: number }[];
  files: { name: string; size: number }[];
  /** Read-only hosts (the shared work-item detail) omit the write callbacks. */
  readOnly: boolean;
  onWrite?: Props["onWrite"];
  onError?: Props["onError"];
}) {
  const empty = images.length === 0 && files.length === 0;
  return (
    <section className="grid gap-4">
      {empty && (
        <div className="card px-4 py-8 text-center">
          <div className="text-cardtitle font-semibold text-ink-100">
            No evidence yet
          </div>
          <p className="text-secondary text-ink-400 mt-1">
            Add screenshots, test results, and other supporting files.
          </p>
        </div>
      )}
      {images.length > 0 && (
        <div className="grid gap-2">
          <h2 className="text-cardtitle font-semibold text-ink-100 m-0">
            Screenshots
          </h2>
          <div className="grid gap-3 sm:grid-cols-2 lg:grid-cols-3">
            {images.map((f) => (
              <figure key={f.name} className="card overflow-hidden m-0">
                <Artifact key={`${id}:${f.name}`} id={id} name={f.name} image />
                <figcaption className="flex justify-between gap-2 px-2.5 py-2 text-label">
                  <span className="num truncate" title={f.name}>
                    {f.name}
                  </span>
                  <span className="text-ink-500 shrink-0">
                    {fmtBytes(f.size)}
                  </span>
                </figcaption>
              </figure>
            ))}
          </div>
        </div>
      )}
      {files.length > 0 && (
        <div className="card">
          <div className="slabel px-3 pt-2.5">Artifacts</div>
          <ul>
            {files.map((f) => (
              <li
                key={f.name}
                className="flex items-center gap-3 px-3 py-2 border-t border-ink-800"
              >
                <Artifact key={`${id}:${f.name}`} id={id} name={f.name} />
                <span className="num text-micro text-ink-500 ml-auto shrink-0">
                  {fmtBytes(f.size)}
                </span>
              </li>
            ))}
          </ul>
        </div>
      )}
      {!readOnly && onWrite && onError && <Attach id={id} onWrite={onWrite} onError={onError} />}
    </section>
  );
}
