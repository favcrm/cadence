import { useEffect, useRef, useState } from "react";
import { api } from "../../lib/api";
import { fmtBytes } from "../../lib/fmt";
import { useTranslate } from "../../lib/locale";
import {
  beginPrivateOperation,
  onPrivateReadsPaused,
  privateFetch,
  privateReadsAllowed,
  privateSessionGeneration,
  type PrivateOperation,
} from "../../lib/privateSession";
import { sessionHeaders, sessionKey } from "../../lib/sessionKey";
import ArtifactView, {
  openPdfInTab,
  saveBytes,
  type HtmlView,
} from "../canvas/ArtifactView";
import type { ArtifactKind } from "../canvas/canvasStore";
import { Attach } from "./Attach";
import { type IssuePageProps as Props } from "./issuePageProps";

export type DeliverablePreviewKind = Exclude<ArtifactKind, "image">;

const PREVIEW_MIME: Record<DeliverablePreviewKind, string> = {
  md: "text/markdown",
  html: "text/html",
  csv: "text/csv",
  pdf: "application/pdf",
};

const PREVIEW_FOOT: Record<DeliverablePreviewKind, string> = {
  html: "sandboxed frame · no scripts · no network",
  md: "board Markdown renderer · no raw HTML",
  csv: "table · first 2,000 rows · 50 columns",
  pdf: "browser PDF tab or download",
};

const HTML_VIEWS: readonly [HtmlView, string][] = [
  ["rendered", "Rendered"],
  ["source", "HTML"],
];

export function deliverablePreviewKind(name: string): DeliverablePreviewKind | null {
  const clean = name.replace(/^hosted:/, "").toLowerCase();
  if (clean.endsWith(".md") || clean.endsWith(".markdown")) return "md";
  if (clean.endsWith(".html") || clean.endsWith(".htm")) return "html";
  if (clean.endsWith(".csv")) return "csv";
  if (clean.endsWith(".pdf")) return "pdf";
  return null;
}

export function deliverableDisplayName(name: string): string {
  const clean = name.replace(/^hosted:/, "");
  const linked = /^(.*?)--art-[a-z0-9]{1,40}-v(\d+)(\.[a-z0-9]+)$/i.exec(clean);
  if (!linked) return clean;
  return `${linked[1]} (v${linked[2]})${linked[3]}`;
}

export function rasterMimeFromBytes(bytes: Uint8Array): string | null {
  if (
    bytes.length >= 8 &&
    bytes[0] === 0x89 &&
    bytes[1] === 0x50 &&
    bytes[2] === 0x4e &&
    bytes[3] === 0x47 &&
    bytes[4] === 0x0d &&
    bytes[5] === 0x0a &&
    bytes[6] === 0x1a &&
    bytes[7] === 0x0a
  ) {
    return "image/png";
  }
  if (bytes.length >= 3 && bytes[0] === 0xff && bytes[1] === 0xd8 && bytes[2] === 0xff) {
    return "image/jpeg";
  }
  if (
    bytes.length >= 6 &&
    bytes[0] === 0x47 &&
    bytes[1] === 0x49 &&
    bytes[2] === 0x46 &&
    bytes[3] === 0x38 &&
    (bytes[4] === 0x37 || bytes[4] === 0x39) &&
    bytes[5] === 0x61
  ) {
    return "image/gif";
  }
  if (
    bytes.length >= 12 &&
    bytes[0] === 0x52 &&
    bytes[1] === 0x49 &&
    bytes[2] === 0x46 &&
    bytes[3] === 0x46 &&
    bytes[8] === 0x57 &&
    bytes[9] === 0x45 &&
    bytes[10] === 0x42 &&
    bytes[11] === 0x50
  ) {
    return "image/webp";
  }
  return null;
}

interface LoadedArtifact {
  id: string;
  name: string;
  key: string | null;
  generation: number;
  attempt: number;
  url: string;
  preview: string | null;
  blob: Blob;
}

function useLoadedArtifact(id: string, name: string, image: boolean) {
  const key = sessionKey();
  const generation = privateSessionGeneration();
  const [attempt, setAttempt] = useState(0);
  const [loaded, setLoaded] = useState<LoadedArtifact | null>(null);
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
    const current = () =>
      !abort.signal.aborted &&
      !session.signal.aborted &&
      privateReadsAllowed() &&
      privateSessionGeneration() === generation &&
      sessionKey() === key;
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
    privateFetch(
      api.artifactUrl(id, name),
      {
        credentials: "same-origin",
        headers: sessionHeaders(),
        signal: abort.signal,
      },
      session,
    )
      .then(async (response) => {
        if (!response.ok) throw new Error("Artifact could not be loaded");
        const blob = await response.blob();
        session.assertCurrent();
        if (!current()) return;
        // Only raster images may open inline. Hosted boards serve all ticket
        // artifacts as application/octet-stream, so verify raster magic bytes
        // before typing an untyped/octet-stream image blob for data: preview.
        let typedBlob = blob;
        let raster = /^image\/(?:png|jpeg|gif|webp)$/i.test(blob.type);
        if (image && !raster && (!blob.type || blob.type === "application/octet-stream")) {
          const headSlice = blob.slice(0, 12);
          const headBuf =
            typeof headSlice.arrayBuffer === "function"
              ? await headSlice.arrayBuffer()
              : await new Response(headSlice).arrayBuffer();
          session.assertCurrent();
          if (!current()) return;
          const sniffed = rasterMimeFromBytes(new Uint8Array(headBuf));
          if (sniffed) {
            typedBlob = blob.slice(0, blob.size, sniffed);
            raster = true;
          }
        }
        url = URL.createObjectURL(
          raster ? typedBlob : blob.slice(0, blob.size, "application/octet-stream"),
        );
        const ready = (preview: string | null) => {
          if (current() && url) {
            setLoaded({ id, name, key, generation, attempt, url, preview, blob: typedBlob });
          }
        };
        if (image && raster) {
          // The board CSP allows data: images, but not blob: image sources.
          reader = new FileReader();
          reader.onload = () => {
            if (typeof reader?.result === "string") ready(reader.result);
          };
          reader.onerror = () => {
            if (current()) setFailed(true);
          };
          reader.readAsDataURL(typedBlob);
        } else {
          ready(null);
        }
      })
      .catch(() => {
        if (current()) setFailed(true);
      });
    return () => {
      stop();
      session.dispose();
      dispose();
    };
  }, [id, name, image, key, generation, attempt]);

  const ready =
    loaded &&
    loaded.id === id &&
    loaded.name === name &&
    loaded.key === key &&
    loaded.generation === generation &&
    loaded.attempt === attempt &&
    privateReadsAllowed()
      ? loaded
      : null;

  return {
    ready,
    failed,
    setFailed,
    retry: () => setAttempt((value) => value + 1),
  };
}

function ImageArtifact({ id, name }: { id: string; name: string }) {
  const t = useTranslate();
  const displayName = deliverableDisplayName(name);
  const { ready, failed, setFailed, retry } = useLoadedArtifact(id, name, true);
  if (ready && !failed) {
    return (
      <a
        className="block min-w-0 truncate"
        href={ready.url}
        download={ready.preview ? undefined : displayName}
        target={ready.preview ? "_blank" : undefined}
        rel={ready.preview ? "noreferrer" : undefined}
        title={name}
      >
        {ready.preview ? (
          <img
            src={ready.preview}
            alt={displayName}
            className="w-full h-28 object-cover bg-ink-900"
            onError={() => setFailed(true)}
          />
        ) : (
          displayName
        )}
      </a>
    );
  }
  return (
    <div className="grid min-w-0 min-h-28 gap-2 p-2.5 text-label">
      {failed ? (
        <div role="alert">
          <span>
            {t("Could not load")} {displayName}.{" "}
          </span>
          <button
            type="button"
            className="lnk"
            aria-label={`${t("Retry")} ${displayName}`}
            onClick={retry}
          >
            {t("Retry")}
          </button>
        </div>
      ) : (
        <span role="status">
          {t("Loading")} {displayName}…
        </span>
      )}
    </div>
  );
}

function FileArtifactRow({
  id,
  name,
  size,
}: {
  id: string;
  name: string;
  size: number;
}) {
  const t = useTranslate();
  const { ready, failed, retry } = useLoadedArtifact(id, name, false);
  const previewKind = deliverablePreviewKind(name);
  const displayName = deliverableDisplayName(name);
  const [previewOpen, setPreviewOpen] = useState(false);
  const [htmlView, setHtmlView] = useState<HtmlView>("rendered");
  const [previewBytes, setPreviewBytes] = useState<ArrayBuffer | null>(null);
  const contentRef = useRef<HTMLDivElement | null>(null);

  useEffect(() => {
    const stop = onPrivateReadsPaused(() => {
      setPreviewOpen(false);
      setPreviewBytes(null);
    });
    return stop;
  }, []);

  useEffect(() => {
    if (!previewOpen || !ready || !previewKind) {
      setPreviewBytes(null);
      return;
    }
    let cancelled = false;
    const readBlob =
      typeof ready.blob.arrayBuffer === "function"
        ? ready.blob.arrayBuffer()
        : new Response(ready.blob).arrayBuffer();
    readBlob
      .then((buffer) => {
        if (!cancelled && privateReadsAllowed()) setPreviewBytes(buffer);
      })
      .catch(() => {
        if (!cancelled) setPreviewBytes(null);
      });
    return () => {
      cancelled = true;
    };
  }, [previewOpen, ready, previewKind]);

  const showPreview = Boolean(previewOpen && ready && !failed && previewKind);

  return (
    <li className="min-w-0 border-t border-ink-800">
      <div className="flex min-w-0 items-center gap-3 px-3 py-2">
        {ready && !failed ? (
          <>
            <a
              className="lnk num text-label truncate min-w-0"
              href={ready.url}
              download={displayName}
              title={name}
            >
              {displayName}
            </a>
            {previewKind && (
              <button
                type="button"
                className="lnk text-micro shrink-0"
                aria-expanded={previewOpen}
                aria-label={`${t(previewOpen ? "Hide preview" : "Preview")} ${displayName}`}
                onClick={() => setPreviewOpen((open) => !open)}
              >
                {t(previewOpen ? "Hide preview" : "Preview")}
              </button>
            )}
          </>
        ) : (
          <div className="min-w-0 truncate text-label">
            <span className="num truncate" title={name}>
              {displayName}
            </span>{" "}
            {failed ? (
              <div role="alert">
                <span>
                  {t("Could not load")} {displayName}.{" "}
                </span>
                <button
                  type="button"
                  className="lnk"
                  aria-label={`${t("Retry")} ${displayName}`}
                  onClick={retry}
                >
                  {t("Retry")}
                </button>
              </div>
            ) : (
              <span role="status">
                {t("Loading")} {displayName}…
              </span>
            )}
          </div>
        )}
        <span className="num text-micro text-ink-500 ml-auto shrink-0">
          {fmtBytes(size)}
        </span>
      </div>
      {showPreview && previewKind && (
        <div
          className="issue-deliverable-preview"
          role="region"
          aria-label={`${t("Preview")} ${displayName}`}
          data-deliverable-preview={previewKind}
        >
          <div className="cv-head">
            <span className="arttype">{previewKind.toUpperCase()}</span>
            <div className="cv-title">
              <b title={displayName}>{displayName}</b>
              <span>{fmtBytes(size)}</span>
            </div>
            <div className="flex flex-wrap items-center gap-2">
              {previewKind === "html" && (
                <div
                  className="vers cv-view"
                  role="group"
                  aria-label={t("Document view")}
                >
                  {HTML_VIEWS.map(([mode, label]) => (
                    <button
                      key={mode}
                      type="button"
                      aria-pressed={htmlView === mode}
                      onClick={() => setHtmlView(mode)}
                    >
                      {t(label)}
                    </button>
                  ))}
                </div>
              )}
              {previewKind === "pdf" && previewBytes && (
                <button
                  type="button"
                  className="lnk text-micro"
                  onClick={() => openPdfInTab(previewBytes)}
                >
                  {t("Open in new tab")}
                </button>
              )}
              {previewBytes && (
                <button
                  type="button"
                  className="lnk text-micro"
                  onClick={() =>
                    saveBytes(
                      previewBytes,
                      PREVIEW_MIME[previewKind],
                      displayName,
                    )
                  }
                >
                  {t("Download")}
                </button>
              )}
              <button
                type="button"
                className="lnk text-micro"
                onClick={() => setPreviewOpen(false)}
              >
                {t("Close")}
              </button>
            </div>
          </div>
          <div className="cv-body">
            {previewBytes ? (
              <ArtifactView
                kind={previewKind}
                title={displayName}
                data={{ bytes: previewBytes, mime: PREVIEW_MIME[previewKind] }}
                contentRef={contentRef}
                htmlView={htmlView}
              />
            ) : (
              <span className="text-micro text-ink-400" role="status">
                {t("Loading")} {displayName}…
              </span>
            )}
          </div>
          <div className="cv-foot">{t(PREVIEW_FOOT[previewKind])}</div>
        </div>
      )}
    </li>
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
  const t = useTranslate();
  const empty = images.length === 0 && files.length === 0;
  return (
    <section className="grid min-w-0 gap-4">
      {empty && (
        <div className="card min-w-0 px-4 py-8 text-center">
          <div className="text-cardtitle font-semibold text-ink-100">
            {t("No evidence yet")}
          </div>
          <p className="text-secondary text-ink-400 mt-1">
            {t("Add screenshots, test results, and other supporting files.")}
          </p>
        </div>
      )}
      {images.length > 0 && (
        <div className="grid min-w-0 gap-2">
          <h2 className="text-cardtitle font-semibold text-ink-100 m-0">
            {t("Screenshots")}
          </h2>
          <div className="grid min-w-0 gap-3 sm:grid-cols-2 lg:grid-cols-3">
            {images.map((f) => {
              const displayName = deliverableDisplayName(f.name);
              return (
                <figure key={f.name} className="card min-w-0 overflow-hidden m-0">
                  <ImageArtifact key={`${id}:${f.name}`} id={id} name={f.name} />
                  <figcaption className="flex min-w-0 justify-between gap-2 px-2.5 py-2 text-label">
                    <span className="num truncate min-w-0" title={f.name}>
                      {displayName}
                    </span>
                    <span className="text-ink-500 shrink-0">
                      {fmtBytes(f.size)}
                    </span>
                  </figcaption>
                </figure>
              );
            })}
          </div>
        </div>
      )}
      {files.length > 0 && (
        <div className="card min-w-0 overflow-hidden">
          <div className="slabel px-3 pt-2.5">{t("Artifacts")}</div>
          <ul className="list-none m-0 p-0 min-w-0">
            {files.map((f) => (
              <FileArtifactRow
                key={`${id}:${f.name}`}
                id={id}
                name={f.name}
                size={f.size}
              />
            ))}
          </ul>
        </div>
      )}
      {!readOnly && onWrite && onError && (
        <Attach id={id} onWrite={onWrite} onError={onError} />
      )}
    </section>
  );
}
