import { useEffect, useRef, useState } from "react";
import { createPortal } from "react-dom";
import { useTranslate } from "../../lib/locale";
import { attachMeta } from "../app-shell/chat/attach";

/**
 * CAD-2219: the operator's own attachment, shown as what it is.
 *
 * A raster gets a capped thumbnail that opens a lightbox; a clip plays inline
 * with the browser's own controls. Both read the same-origin, scoped route the
 * board serves the attachment from, so the request carries the session cookie
 * and the conversation — never a token in a URL the page could leak.
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
  void fetch(attachmentHref(id, conversation), { credentials: "same-origin" })
    .then(async (response) => {
      if (!response.ok) throw new Error(`attachment read ${response.status}`);
      const href = URL.createObjectURL(await response.blob());
      tab.location.replace(href);
      window.setTimeout(() => URL.revokeObjectURL(href), 30_000);
    })
    .catch(() => tab.close());
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
  const t = useTranslate();
  const [openId, setOpenId] = useState<string | null>(null);
  const [failed, setFailed] = useState<Record<string, true>>({});
  const shown =
    hosted && conversation !== null
      ? files.flatMap((file) => {
          const kind = attachmentPreviewKind(file.mime);
          return kind === null || failed[file.id] === true ? [] : [{ file, kind }];
        })
      : [];
  const images = shown.flatMap((entry) => (entry.kind === "image" ? [entry.file] : []));
  const openIndex = openId === null ? -1 : images.findIndex((image) => image.id === openId);
  useEffect(() => {
    if (openId !== null && openIndex < 0) setOpenId(null);
  }, [openId, openIndex]);
  if (!hosted || conversation === null || shown.length === 0) return null;
  return (
    <div className="sentmedia" data-entry-media>
      {shown.map(({ file, kind }) =>
        kind === "video" ? (
          <figure key={file.id} className="sentclip">
            <video
              className="sentvideo"
              src={attachmentHref(file.id, conversation)}
              controls
              playsInline
              preload="metadata"
              onError={() => setFailed((state) => ({ ...state, [file.id]: true }))}
            />
          </figure>
        ) : (
          <button
            key={file.id}
            type="button"
            className="sentthumb"
            aria-label={`${t("Open")} ${file.name}`}
            onClick={() => setOpenId(file.id)}
          >
            <img
              src={attachmentHref(file.id, conversation)}
              alt={file.name}
              loading="lazy"
              decoding="async"
              onError={() => setFailed((state) => ({ ...state, [file.id]: true }))}
            />
          </button>
        ),
      )}
      {openIndex >= 0 && (
        <AttachmentLightbox
          images={images}
          index={openIndex}
          conversation={conversation}
          onSelect={setOpenId}
          onClose={() => setOpenId(null)}
        />
      )}
    </div>
  );
}

/** The one picture at a readable size, with its name, size and a Download.
 *  When a message carries more than one picture, Previous/Next and the arrow
 *  keys step through them inside the trapped modal. */
function AttachmentLightbox({
  images,
  index,
  conversation,
  onSelect,
  onClose,
}: {
  images: AttachmentFile[];
  index: number;
  conversation: string;
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
        if (next) nav.current.onSelect(next.id);
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
  const file = images[index];
  const src = attachmentHref(file.id, conversation);
  const multi = images.length > 1;
  useEffect(() => {
    const active = document.activeElement;
    if (!multi && dialogRef.current && (active === null || !dialogRef.current.contains(active))) {
      closeRef.current?.focus();
    }
  }, [multi]);
  const step = (delta: -1 | 1) => {
    const next = images[(index + delta + images.length) % images.length];
    if (next) onSelect(next.id);
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
          <a className="lnk" href={src} download={file.name}>
            {t("Download")}
          </a>
          <button ref={closeRef} type="button" className="lnk" onClick={() => dismiss.current()}>
            {t("Close")}
          </button>
        </div>
      </div>
    </div>,
    document.body,
  );
}
