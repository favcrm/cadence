import { useTranslate } from "../../lib/locale";
import { sizeLabel } from "./ArtifactCard";

/** The format a mime names, upper-cased for the caption: `image/png` is PNG,
 *  `image/svg+xml` is SVG. Empty when the mime names nothing. */
export function formatLabel(mime: string): string {
  const sub = mime.split(";")[0].split("/")[1] ?? "";
  return sub.split("+")[0].toUpperCase();
}

/** The caption's second line: shape, format and size, as far as each is known. */
export function imageDetails(opts: { aspect?: string; mime: string; bytes: number }): string {
  return [opts.aspect, formatLabel(opts.mime), sizeLabel(opts.bytes)].filter(Boolean).join(" · ");
}

/** The scrim on a picture: its title, details and a Download. One component
 *  for the thread and the canvas. It sits on the picture, not on the theme,
 *  so its colours are fixed (see `.imgover` in canvas.css). */
export default function ImageOverlay({ title, aspect, mime, bytes, onDownload }: {
  title: string;
  aspect?: string;
  mime: string;
  bytes: number;
  onDownload: () => void;
}) {
  const t = useTranslate();
  return (
    <div className="imgover" role="group" aria-label={title} data-image-overlay>
      <div className="imgcap">
        <b>{title}</b>
        <span>{imageDetails({ aspect, mime, bytes })}</span>
      </div>
      <button type="button" className="imgtool" aria-label={t("Download")} title={t("Download")} onClick={onDownload}>
        <svg viewBox="0 0 24 24" width="16" height="16" fill="none" stroke="currentColor" strokeWidth="1.8" strokeLinecap="round" strokeLinejoin="round" aria-hidden>
          <path d="M12 4v11m0 0-4-4m4 4 4-4M5 20h14" />
        </svg>
      </button>
    </div>
  );
}
