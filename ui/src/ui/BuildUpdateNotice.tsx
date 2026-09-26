import { useEffect, useId, useRef, useState } from "react";
import { IconUpdate } from "./icons";

/** Quiet update affordance in the header; actions appear only on demand. */
export default function BuildUpdateNotice({ onReload, onDismiss }: {
  onReload: () => void;
  onDismiss: () => void;
}) {
  const tooltipId = useId();
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
        className="header-update-trigger header-icon text-accent" aria-label="Update available"
        aria-expanded={open} aria-controls={open ? panelId : undefined} aria-describedby={tooltipId}>
        <IconUpdate />
        <span className="hidden sm:inline">Update available</span>
      </button>
      <span id={tooltipId} role="tooltip" className="header-tooltip">Update available</span>
      {open && <div id={panelId} className="header-update-panel card p-4 shadow-lg">
        <p className="text-secondary font-medium text-ink-100">Cadence was updated</p>
        <p className="mt-1 text-label text-ink-400">Reload to use the latest version.</p>
        <div className="mt-3 flex items-center justify-end gap-1">
          <button type="button" onClick={onDismiss} aria-label="Dismiss update notification" className="build-update-action text-ink-400">Later</button>
          <button type="button" onClick={onReload} className="build-update-action bg-ink-800 text-ink-100">Reload</button>
        </div>
      </div>}
    </div>
  );
}
