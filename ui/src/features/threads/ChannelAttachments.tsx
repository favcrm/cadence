import { useMemo, useState } from "react";
import { useTranslate } from "../../lib/locale";
import { attachMeta } from "../app-shell/chat/attach";
import AttachmentMedia, { attachmentPreviewKind, openAttachmentUrlInTab } from "../home/AttachmentMedia";
import type { HumanPostAttachment } from "./humanThreadsClient";

function extTag(name: string): string {
  const parts = name.split(".");
  const ext = parts.length > 1 ? parts[parts.length - 1]!.trim() : "";
  return ext ? ext.toUpperCase().slice(0, 5) : "FILE";
}

function chipDataKind(att: HumanPostAttachment): string {
  if (att.kind === "pdf" || att.mime === "application/pdf") return "pdf";
  if (att.kind === "image" || att.kind === "video") return "media";
  return "text";
}

/**
 * CAD-2841: render room-scoped attachment previews and download chips on a
 * Hosted Channel or DM post. Images render rounded thumbnails with lightbox;
 * videos play inline; PDFs open via blob fetch in a new tab; other files
 * (CSV, text, Office docs) render compact download chips.
 */
export default function ChannelAttachments({
  attachments,
  mine = false,
}: {
  attachments: readonly HumanPostAttachment[];
  mine?: boolean;
}) {
  const t = useTranslate();
  const [failedMedia, setFailedMedia] = useState<Record<string, true>>({});

  const { mediaFiles, chipFiles, urlById } = useMemo(() => {
    const urls = new Map<string, string>();
    const media: HumanPostAttachment[] = [];
    const chips: HumanPostAttachment[] = [];
    for (const att of attachments) {
      urls.set(att.id, att.url);
      const previewKind = attachmentPreviewKind(att.mime, true);
      if (previewKind !== null && failedMedia[att.id] !== true) {
        media.push(att);
      } else {
        chips.push(att);
      }
    }
    return { mediaFiles: media, chipFiles: chips, urlById: urls };
  }, [attachments, failedMedia]);

  if (attachments.length === 0) return null;

  return (
    <div
      className={mine ? "threads-attachments is-me" : "threads-attachments"}
      data-channel-attachments=""
    >
      {mediaFiles.length > 0 && (
        <AttachmentMedia
          files={mediaFiles}
          hosted
          allowSvgRaster
          hrefFor={(file) => urlById.get(file.id) ?? ""}
          onMediaError={(id) => setFailedMedia((prev) => ({ ...prev, [id]: true }))}
        />
      )}
      {chipFiles.length > 0 && (
        <div className="sentchips" data-entry-attachments="">
          {chipFiles.map((f) => {
            const title = `${f.name} · ${f.size} B · ${f.mime}`;
            const body = (
              <>
                <span className="sentchip-ext" aria-hidden="true">
                  {extTag(f.name)}
                </span>
                <span className="sentchip-text">
                  <span className="sentchip-name">{f.name}</span>
                  <span className="sentchip-meta">{attachMeta(f.name, f.size, t)}</span>
                </span>
              </>
            );
            if (f.mime === "application/pdf") {
              return (
                <button
                  key={f.id}
                  type="button"
                  className="sentchip threads-file-chip"
                  data-kind="pdf"
                  title={`${title} — ${t("(opens in a new tab)")}`}
                  onClick={() => openAttachmentUrlInTab(f.url)}
                >
                  {body}
                </button>
              );
            }
            return (
              <a
                key={f.id}
                className="sentchip threads-file-chip"
                href={f.url}
                download={f.name}
                data-kind={chipDataKind(f)}
                title={title}
              >
                {body}
              </a>
            );
          })}
        </div>
      )}
    </div>
  );
}
