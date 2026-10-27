import { useEffect, useId, useRef, useState } from "react";
import { IconUpdate } from "./icons";
import { useTranslate } from "../lib/locale";

/** Quiet update affordance in the header; actions appear only on demand. */
export default function BuildUpdateNotice({ onReload, onDismiss }: {
  onReload: () => void;
  onDismiss: () => void;
}) {
  const t = useTranslate();
  const panelId = useId();
  const [open, setOpen] = useState(false);
  const noticeRef = useRef<HTMLDivElement>(null);
  useEffect(() => {
    const closeOutside = (event: PointerEvent) => {
      if (event.target instanceof Node && !noticeRef.current?.contains(event.target)) setOpen(false);
    };
    document.addEventListener("pointerdown", closeOutside);
    return () => document.removeEventListener("pointerdown", closeOutside);
  }, []);
  return (
    <div ref={noticeRef} className="header-update header-control-wrap" data-build-banner data-open={open}
      onKeyDown={(event) => {
        if (event.key === "Escape") {
          setOpen(false);
          event.currentTarget.querySelector("button")?.focus();
        }
      }}>
      <button type="button" onClick={() => setOpen((value) => !value)}
        className="header-update-trigger header-state" aria-label={t("Update available")}
        aria-expanded={open} aria-controls={open ? panelId : undefined}>
        <span className="text-accent"><IconUpdate size={13} /></span>
        <span className="hidden sm:inline">{t("Update")}</span>
      </button>
      <span role="tooltip" className="header-tooltip sm:hidden">{t("Update available")}</span>
      {open && <div id={panelId} className="header-update-panel card p-4 shadow-lg">
        <p className="text-secondary font-medium text-ink-100">{t("Cadence was updated")}</p>
        <p className="mt-1 text-label text-ink-400">{t("Reload to use the latest version.")}</p>
        <div className="mt-3 flex items-center justify-end gap-1">
          <button type="button" onClick={onDismiss} aria-label={t("Dismiss update notification")} className="build-update-action text-ink-400">{t("Later")}</button>
          <button type="button" onClick={onReload} className="build-update-action bg-ink-800 text-ink-100">{t("Reload")}</button>
        </div>
      </div>}
    </div>
  );
}
