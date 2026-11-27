import { useEffect, useId, useRef, useState } from "react";
import { useTranslate } from "../../lib/locale";
import { IconClose } from "../../ui/icons";
import { sizeLabel } from "./ArtifactCard";
import { openPdfInTab, saveBytes } from "./ArtifactView";
import PdfDocumentView from "./PdfDocumentView";

/** Wide/full-canvas slide-over PDF overlay drawer used both over Canvas views and full-page app screens. */
export default function PdfOverlayDrawer({
  title,
  bytes,
  mime = "application/pdf",
  fileName,
  loading = false,
  error = null,
  onClose,
  scope = "canvas",
}: {
  title: string;
  bytes: ArrayBuffer | null;
  mime?: string;
  fileName?: string;
  loading?: boolean;
  error?: string | null;
  onClose: () => void;
  scope?: "canvas" | "screen";
}) {
  const t = useTranslate();
  const titleId = useId();
  const [fullWidth, setFullWidth] = useState(false);
  const closeRef = useRef<HTMLButtonElement | null>(null);
  const previousFocusRef = useRef<HTMLElement | null>(null);

  useEffect(() => {
    previousFocusRef.current = document.activeElement instanceof HTMLElement ? document.activeElement : null;
    closeRef.current?.focus({ preventScroll: true });
    return () => {
      const prev = previousFocusRef.current;
      if (prev && prev.isConnected) {
        prev.focus({ preventScroll: true });
      }
    };
  }, []);

  useEffect(() => {
    const onKeyDown = (e: KeyboardEvent) => {
      if (e.key !== "Escape") return;
      e.preventDefault();
      e.stopImmediatePropagation();
      onClose();
    };
    window.addEventListener("keydown", onKeyDown, true);
    return () => window.removeEventListener("keydown", onKeyDown, true);
  }, [onClose]);

  const displayName = fileName?.trim() || title;
  const downloadTargetName = displayName.toLowerCase().endsWith(".pdf") ? displayName : `${displayName}.pdf`;
  const widthLabel = t(fullWidth ? "Default width" : "Full width");

  return (
    <div className="cv-pdf-overlay" data-pdf-overlay data-scope={scope}>
      <div className="cv-pdf-backdrop" aria-hidden="true" onClick={onClose} />
      <section
        className="cv-pdf-drawer"
        role="dialog"
        aria-modal="true"
        aria-labelledby={titleId}
        data-full={fullWidth ? "true" : undefined}
      >
        <header className="cv-pdf-head">
          <span className="arttype cv-pdf-badge">PDF</span>
          <div className="cv-pdf-title">
            <h2 id={titleId} title={displayName}>{displayName}</h2>
            <p className="num text-micro text-ink-400">
              {loading
                ? t("Loading PDF preview…")
                : error
                  ? "PDF"
                  : bytes
                    ? `PDF · ${sizeLabel(bytes.byteLength)}`
                    : "PDF"}
            </p>
          </div>
          <div className="cv-pdf-actions">
            <button
              type="button"
              className="btn btn-ghost cv-pdf-width-btn"
              aria-pressed={fullWidth}
              aria-label={widthLabel}
              title={widthLabel}
              onClick={() => setFullWidth((v) => !v)}
            >
              <svg viewBox="0 0 24 24" width="14" height="14" fill="none" stroke="currentColor" strokeWidth="1.8" strokeLinecap="round" strokeLinejoin="round" aria-hidden="true">
                {fullWidth ? (
                  <path d="M9 4v5H4M15 4v5h5M9 20v-5H4M15 20v-5h5" />
                ) : (
                  <path d="M4 9V4h5M20 9V4h-5M4 15v5h5M20 15v5h-5" />
                )}
              </svg>
              <span>{widthLabel}</span>
            </button>
            {bytes ? (
              <>
                <button
                  type="button"
                  className="btn btn-ghost"
                  onClick={() => openPdfInTab(bytes)}
                >
                  {t("Open in new tab")}
                </button>
                <button
                  type="button"
                  className="btn"
                  onClick={() => saveBytes(bytes, mime, downloadTargetName)}
                >
                  {t("Download")}
                </button>
              </>
            ) : null}
            <button
              ref={closeRef}
              type="button"
              className="cv-iconbtn"
              aria-label={t("Close")}
              title={t("Close")}
              onClick={onClose}
            >
              <IconClose size={14} />
            </button>
          </div>
        </header>
        <div className="cv-pdf-body">
          {loading ? (
            <p className="text-label text-ink-400 p-4" role="status">{t("Loading PDF preview…")}</p>
          ) : error ? (
            <p className="text-label text-fail p-4" role="alert">{error}</p>
          ) : bytes ? (
            <PdfDocumentView
              title={displayName}
              bytes={bytes}
              onOpenTab={() => openPdfInTab(bytes)}
              onDownload={() => saveBytes(bytes, mime, downloadTargetName)}
              compact
            />
          ) : null}
        </div>
      </section>
    </div>
  );
}
