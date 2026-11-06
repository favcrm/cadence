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
  const [open, setOpen] = useState<AttachmentFile | null>(null);
  const [failed, setFailed] = useState<Record<string, true>>({});
  if (!hosted || conversation === null) return null;
  const shown = files.flatMap((file) => {
    const kind = attachmentPreviewKind(file.mime);
    return kind === null || failed[file.id] === true ? [] : [{ file, kind }];
  });
  if (shown.length === 0) return null;
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
            onClick={() => setOpen(file)}
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
      {open !== null && (
        <AttachmentLightbox
          file={open}
          src={attachmentHref(open.id, conversation)}
          onClose={() => setOpen(null)}
        />
      )}
    </div>
  );
}

/** The one picture at a readable size, with its name, size and a Download. */
function AttachmentLightbox({
  file,
  src,
  onClose,
}: {
  file: AttachmentFile;
  src: string;
  onClose: () => void;
}) {
  const t = useTranslate();
  const closeRef = useRef<HTMLButtonElement | null>(null);
  const dismiss = useRef(onClose);
  dismiss.current = onClose;
  useEffect(() => {
    const returnTo = document.activeElement as HTMLElement | null;
    closeRef.current?.focus();
    const onKey = (event: KeyboardEvent) => {
      if (event.key === "Escape") dismiss.current();
    };
    document.addEventListener("keydown", onKey);
    return () => {
      document.removeEventListener("keydown", onKey);
      returnTo?.focus?.();
    };
  }, []);
  return createPortal(
    <div
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
