import { useCallback, useEffect, useRef, useState } from "react";
import { createPortal } from "react-dom";
import { requireHostedMediaWorker } from "../../lib/hostedMediaSession";
import { useTranslate } from "../../lib/locale";
import { onPrivateReadsPaused, privateReadsAllowed, verifiedHostedSessionId, withPrivateOperation } from "../../lib/privateSession";
import { attachMeta } from "../app-shell/chat/attach";
import { useNear } from "../canvas/ImageBlock";

/**
 * CAD-2219: the operator's own attachment, shown as what it is.
 *
 * A raster gets a capped thumbnail that opens a lightbox; a clip plays inline
 * with the browser's own controls. Images use verified response headers; the
 * service worker pins each video range to this tab's verified session.
 *
 * The chip row stays: the thumbnail is an addition, and a load that fails
 * (older board, another conversation, bytes gone) removes it and leaves the
 * metadata chips as the only surface.
 */
const IMAGE_MIMES: Record<string, true> = {
  "image/png": true,
  "image/jpeg": true,
  "image/webp": true,
};
const VIDEO_MIMES: Record<string, true> = {
  "video/mp4": true,
  "video/webm": true,
};

export type AttachmentFile = { id: string; name: string; size: number; mime: string };

/** The board read for one attachment, scoped to the conversation in view. */
export function attachmentHref(id: string, conversation: string): string {
  return `/api/chat/attachments/${encodeURIComponent(id)}?conversation=${encodeURIComponent(conversation)}`;
}
function attachmentRangeHref(id: string, conversation: string): string {
  return `/api/chat/attachments-bound/v1/${encodeURIComponent(id)}?conversation=${encodeURIComponent(conversation)}`;
}


function attachmentSourceKey(conversation: string, id: string): string {
  return JSON.stringify([conversation, id]);
}

export function attachmentPreviewKind(mime: string): "image" | "video" | null {
  if (IMAGE_MIMES[mime] === true) return "image";
  if (VIDEO_MIMES[mime] === true) return "video";
  return null;
}

/**
 * CAD-2219: open a retained PDF through the board's read, never by navigating
 * to the route. A refused read must leave the chip inert instead of opening a
 * tab that says "not found". The blank tab is opened inside the click so the
 * later fetch cannot be mistaken for a popup; it is closed when the read fails.
 */
export function openAttachmentInTab(id: string, conversation: string): void {
  const tab = window.open("about:blank", "_blank");
  if (tab === null) return;
  void withPrivateOperation(async (operation) => {
    const expectedSession = verifiedHostedSessionId();
    if (!expectedSession) throw new Error("A verified hosted session is required");
    const response = await operation.fetch(attachmentHref(id, conversation), {
      credentials: "same-origin",
      headers: { "X-Cadence-Expected-Session": expectedSession },
    });
    if (!response.ok) throw new Error(`attachment read ${response.status}`);
    if (response.headers.get("X-Cadence-Verified-Session") !== expectedSession)
      throw new Error("Attachment session verification failed");
    const body = await response.blob();
    operation.assertCurrent();
    if (verifiedHostedSessionId() !== expectedSession)
      throw new Error("The hosted session changed during the attachment read");
    const href = URL.createObjectURL(body);
    try {
      tab.location.replace(href);
    } catch (error) {
      URL.revokeObjectURL(href);
      throw error;
    }
    window.setTimeout(() => URL.revokeObjectURL(href), 30_000);
  }).catch(() => tab.close());
}

export default function AttachmentMedia({
  files,
  conversation,
  hosted,
}: {
  files: AttachmentFile[];
  conversation: string | null;
  /** Only a hosted board serves the attachment read; a local board keeps chips. */
  hosted: boolean;
}) {
  const [openId, setOpenId] = useState<string | null>(null);
  const [failed, setFailed] = useState<Record<string, true>>({});
  const [sources, setSources] = useState<Record<string, string>>({});
  const onSource = useCallback((id: string, source: string | null) => {
    setSources((current) => {
      if (source !== null) return current[id] === source ? current : { ...current, [id]: source };
      if (!(id in current)) return current;
      const next = { ...current };
      delete next[id];
      return next;
    });
  }, []);
  const onFailed = useCallback((id: string) => {
    setFailed((state) => ({ ...state, [id]: true }));
  }, []);
  const shown =
    hosted && conversation !== null
      ? files.flatMap((file) => {
          const kind = attachmentPreviewKind(file.mime);
          if (kind === null) return [];
          const key = attachmentSourceKey(conversation, file.id);
          return failed[key] === true ? [] : [{ file, kind, key }];
        })
      : [];
  const images = shown.filter((entry) => entry.kind === "image");
  const openIndex = openId === null ? -1 : images.findIndex((image) => image.key === openId);
  useEffect(() => {
    if (openId !== null && openIndex < 0) setOpenId(null);
  }, [openId, openIndex]);
  if (!hosted || conversation === null || shown.length === 0) return null;
  return (
    <div className="sentmedia" data-entry-media>
      {shown.map(({ file, kind, key }) =>
        kind === "video" ? (
          <PinnedVideo key={key} file={file} conversation={conversation} sourceKey={key} onFailed={onFailed} />
        ) : (
          <PinnedImage
            key={key}
            file={file}
            conversation={conversation}
            sourceKey={key}
            source={sources[key]}
            selected={openId === key}
            onSource={onSource}
            onFailed={onFailed}
            onOpen={setOpenId}
          />
        ),
      )}
      {openIndex >= 0 && (
        <AttachmentLightbox
          images={images}
          index={openIndex}
          sources={sources}
          onSelect={setOpenId}
          onClose={() => setOpenId(null)}
        />
      )}
    </div>
  );
}
/** A clip keeps range streaming, but only after this tab controls a v1 worker. */
function PinnedVideo({
  file,
  conversation,
  sourceKey,
  onFailed,
}: {
  file: AttachmentFile;
  conversation: string;
  sourceKey: string;
  onFailed: (id: string) => void;
}) {
  const figure = useRef<HTMLElement | null>(null);
  const video = useRef<HTMLVideoElement | null>(null);
  const near = useNear(figure);
  const [ready, setReady] = useState(false);
  const [workerRevision, setWorkerRevision] = useState(0);
  useEffect(() => {
    if (!near) return;
    const abort = new AbortController();
    let active = false;
    const service = typeof navigator !== "undefined" && "serviceWorker" in navigator ? navigator.serviceWorker : null;
    const clear = () => {
      const element = video.current;
      if (!element || !element.hasAttribute("src")) return;
      try { element.pause(); } catch { /* The media element may already be detached. */ }
      element.removeAttribute("src");
      try { element.load(); } catch { /* Removing the source still stops further requests. */ }
    };
    const controllerChanged = () => {
      if (!active || abort.signal.aborted) return;
      abort.abort();
      clear();
      setReady(false);
      setWorkerRevision((revision) => revision + 1);
    };
    service?.addEventListener("controllerchange", controllerChanged);
    const stopPause = onPrivateReadsPaused(() => { abort.abort(); clear(); setReady(false); });
    void withPrivateOperation(async (operation) => {
      const expectedSession = verifiedHostedSessionId();
      if (!expectedSession) throw new Error("A verified hosted session is required");
      await requireHostedMediaWorker(operation.signal);
      operation.assertCurrent();
      if (verifiedHostedSessionId() !== expectedSession)
        throw new Error("The hosted session changed during media setup");
      active = true;
      setReady(true);
    }, abort.signal).catch(() => {
      if (!abort.signal.aborted) onFailed(sourceKey);
    });
    return () => {
      service?.removeEventListener("controllerchange", controllerChanged);
      stopPause();
      abort.abort();
      clear();
      setReady(false);
    };
  }, [near, sourceKey, onFailed, workerRevision]);
  return (
    <figure ref={figure} className="sentclip">
      <video
        ref={video}
        className="sentvideo"
        src={ready ? attachmentRangeHref(file.id, conversation) : undefined}
        crossOrigin="anonymous"
        controls
        playsInline
        preload="metadata"
        onError={() => { if (ready && privateReadsAllowed() && video.current?.hasAttribute("src")) onFailed(sourceKey); }}
      />
    </figure>
  );
}


/** Fetch only near the viewport (or when selected), under this tab's verified session. */
function PinnedImage({
  file,
  conversation,
  sourceKey,
  source,
  selected,
  onSource,
  onFailed,
  onOpen,
}: {
  file: AttachmentFile;
  conversation: string;
  sourceKey: string;
  source: string | undefined;
  selected: boolean;
  onSource: (id: string, source: string | null) => void;
  onFailed: (id: string) => void;
  onOpen: (id: string) => void;
}) {
  const t = useTranslate();
  const ref = useRef<HTMLButtonElement | null>(null);
  const shouldLoad = useNear(ref) || selected;
  useEffect(() => {
    if (!shouldLoad) return;
    const abort = new AbortController();
    let objectUrl: string | null = null;
    const clear = () => {
      if (objectUrl === null) return;
      URL.revokeObjectURL(objectUrl);
      objectUrl = null;
      onSource(sourceKey, null);
    };
    const stopPause = onPrivateReadsPaused(() => { abort.abort(); clear(); });
    void withPrivateOperation(async (operation) => {
      const expectedSession = verifiedHostedSessionId();
      if (!expectedSession) throw new Error("A verified hosted session is required");
      const response = await operation.fetch(attachmentHref(file.id, conversation), {
        credentials: "same-origin",
        headers: { "X-Cadence-Expected-Session": expectedSession },
      });
      if (!response.ok || response.headers.get("X-Cadence-Verified-Session") !== expectedSession)
        throw new Error("Attachment session verification failed");
      const body = await response.blob();
      operation.assertCurrent();
      if (verifiedHostedSessionId() !== expectedSession)
        throw new Error("The hosted session changed during the attachment read");
      objectUrl = URL.createObjectURL(body);
      onSource(sourceKey, objectUrl);
    }, abort.signal).catch(() => {
      if (!abort.signal.aborted) onFailed(sourceKey);
    });
    return () => { stopPause(); abort.abort(); clear(); };
  }, [shouldLoad, sourceKey, file.id, conversation, onSource, onFailed]);
  return (
    <button
      ref={ref}
      type="button"
      className="sentthumb"
      aria-label={`${t("Open")} ${file.name}`}
      disabled={!source}
      onClick={() => onOpen(sourceKey)}
    >
      <img
        src={source}
        alt={file.name}
        loading="lazy"
        decoding="async"
        onError={() => { if (source && privateReadsAllowed()) onFailed(sourceKey); }}
      />
    </button>
  );
}

/** The one picture at a readable size, with its name, size and a Download.
 *  When a message carries more than one picture, Previous/Next and the arrow
 *  keys step through them inside the trapped modal. */
function AttachmentLightbox({
  images,
  index,
  sources,
  onSelect,
  onClose,
}: {
  images: { file: AttachmentFile; key: string }[];
  index: number;
  sources: Record<string, string>;
  onSelect: (id: string) => void;
  onClose: () => void;
}) {
  const t = useTranslate();
  const dialogRef = useRef<HTMLDivElement | null>(null);
  const closeRef = useRef<HTMLButtonElement | null>(null);
  const dismiss = useRef(onClose);
  dismiss.current = onClose;
  const nav = useRef({ images, index, onSelect });
  nav.current = { images, index, onSelect };
  useEffect(() => {
    const returnTo = document.activeElement as HTMLElement | null;
    closeRef.current?.focus();
    const focusables = () =>
      Array.from(
        dialogRef.current?.querySelectorAll<HTMLElement>(
          'button:not([disabled]), [href], input:not([disabled]), select, textarea, [tabindex]:not([tabindex="-1"])',
        ) ?? [],
      );
    const onKey = (event: KeyboardEvent) => {
      if (event.key === "Escape") {
        event.preventDefault();
        dismiss.current();
        return;
      }
      if ((event.key === "ArrowLeft" || event.key === "ArrowRight") && nav.current.images.length > 1) {
        event.preventDefault();
        const count = nav.current.images.length;
        const delta = event.key === "ArrowLeft" ? -1 : 1;
        const next = nav.current.images[(nav.current.index + delta + count) % count];
        if (next) nav.current.onSelect(next.key);
        return;
      }
      if (event.key !== "Tab") return;
      const items = focusables();
      if (items.length === 0) return;
      const first = items[0];
      const last = items[items.length - 1];
      const active = document.activeElement as HTMLElement | null;
      const inside = active !== null && dialogRef.current?.contains(active) === true;
      if (event.shiftKey && (!inside || active === first)) {
        event.preventDefault();
        last.focus();
      } else if (!event.shiftKey && (!inside || active === last)) {
        event.preventDefault();
        first.focus();
      }
    };
    document.addEventListener("keydown", onKey);
    return () => {
      document.removeEventListener("keydown", onKey);
      if (returnTo instanceof HTMLElement && returnTo.isConnected) returnTo.focus();
    };
  }, []);
  const { file, key } = images[index];
  const src = sources[key];
  const multi = images.length > 1;
  useEffect(() => {
    const active = document.activeElement;
    if (!multi && dialogRef.current && (active === null || !dialogRef.current.contains(active))) {
      closeRef.current?.focus();
    }
  }, [multi]);
  const step = (delta: -1 | 1) => {
    const next = images[(index + delta + images.length) % images.length];
    if (next) onSelect(next.key);
  };
  return createPortal(
    <div
      ref={dialogRef}
      className="sentlight"
      role="dialog"
      aria-modal="true"
      aria-label={file.name}
      onClick={() => dismiss.current()}
    >
      <div className="sentlight-inner" onClick={(event) => event.stopPropagation()}>
        <img src={src} alt={file.name} />
        <div className="sentlight-bar">
          <b>{file.name}</b>
          <span className="num">{attachMeta(file.name, file.size, t)}</span>
          {multi && (
            <>
              <span className="num" data-lightbox-count>{`${index + 1} / ${images.length}`}</span>
              <button
                type="button"
                className="lnk"
                aria-label={t("Previous image")}
                onClick={() => step(-1)}
              >
                {t("Previous")}
              </button>
              <button
                type="button"
                className="lnk"
                aria-label={t("Next image")}
                onClick={() => step(1)}
              >
                {t("Next")}
              </button>
            </>
          )}
          {src && (
            <a className="lnk" href={src} download={file.name}>
              {t("Download")}
            </a>
          )}
          <button ref={closeRef} type="button" className="lnk" onClick={() => dismiss.current()}>
            {t("Close")}
          </button>
        </div>
      </div>
    </div>,
    document.body,
  );
}
