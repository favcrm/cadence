import { useLayoutEffect, useRef, type ReactNode } from "react";
import { createPortal } from "react-dom";
import { useLocale } from "../lib/locale";
import { Logo } from "./Logo";
import { IconClose } from "./icons";

/** Modal navigation is outside the inert app shell, including on short screens. */
export default function MobileNavigation({ children, onClose }: { children: ReactNode; onClose: () => void }) {
  const { t } = useLocale();
  const panel = useRef<HTMLElement>(null);
  const closeButton = useRef<HTMLButtonElement>(null);

  useLayoutEffect(() => {
    const opener = document.activeElement instanceof HTMLElement ? document.activeElement : null;
    const shell = document.querySelector<HTMLElement>("[data-app-shell]");
    const wasInert = shell?.inert ?? false;
    const previousOverflow = document.body.style.overflow;
    document.body.style.overflow = "hidden";
    if (shell) shell.inert = true;
    closeButton.current?.focus();
    const onKeyDown = (event: KeyboardEvent) => {
      if (event.defaultPrevented) return;
      if ((event.metaKey || event.ctrlKey) && !event.altKey && !event.shiftKey && event.key.toLowerCase() === "k") {
        // The background search shortcut must not open a portal behind this modal.
        event.preventDefault();
        event.stopPropagation();
        return;
      }
      if (event.key === "Escape") {
        event.preventDefault();
        onClose();
      } else if (event.key === "Tab") {
        const items = Array.from(panel.current?.querySelectorAll<HTMLElement>("a[href], button:not(:disabled), input:not(:disabled), select:not(:disabled), textarea:not(:disabled), [tabindex]:not([tabindex='-1'])") ?? [])
          .filter((item) => !item.closest("[inert], [hidden], [aria-hidden='true']"));
        const first = items[0];
        const last = items[items.length - 1];
        if (!first || !last) {
          event.preventDefault();
          panel.current?.focus();
        } else if (event.shiftKey && (document.activeElement === first || !panel.current?.contains(document.activeElement))) {
          event.preventDefault();
          last.focus();
        } else if (!event.shiftKey && (document.activeElement === last || !panel.current?.contains(document.activeElement))) {
          event.preventDefault();
          first.focus();
        }
      }
    };
    document.addEventListener("keydown", onKeyDown);
    const desktop = window.matchMedia("(min-width: 1024px)");
    const onResize = () => { if (desktop.matches) onClose(); };
    desktop.addEventListener("change", onResize);
    return () => {
      document.removeEventListener("keydown", onKeyDown);
      desktop.removeEventListener("change", onResize);
      if (shell) shell.inert = wasInert;
      opener?.focus();
      document.body.style.overflow = previousOverflow;
    };
  }, [onClose]);

  return createPortal(
    <div className="mobile-navigation-layer">
      <div className="mobile-navigation-scrim" onClick={onClose} aria-hidden="true" />
      <aside ref={panel} id="mobile-navigation" role="dialog" aria-modal="true" aria-label={t("Workspace")} tabIndex={-1} className="mobile-navigation-drawer">
        <div className="mobile-navigation-header">
          <span className="inline-flex items-center gap-2 text-ink-100 text-[17px] font-semibold tracking-[-.035em]"><Logo size={25} />cadence<span className="text-ink-400 font-normal -ml-1">board</span></span>
          <button ref={closeButton} type="button" className="mobile-navigation-close" aria-label={t("Close navigation")} onClick={onClose}><IconClose size={16} /></button>
        </div>
        <div className="mobile-navigation-content">{children}</div>
      </aside>
    </div>, document.body,
  );
}
