import { useEffect, useRef, type ReactNode } from "react";
import Button from "../../ui/Button";
import { IconClose } from "../../ui/icons";
import { useTranslate } from "../../lib/locale";

export function WorkspaceDialog({
  title,
  subtitle,
  badge,
  footer,
  onClose,
  className,
  children,
}: {
  title: string;
  subtitle?: string;
  badge?: string;
  footer?: ReactNode;
  className?: string;
  onClose: () => void;
  children: ReactNode;
}) {
  const t = useTranslate();
  const panel = useRef<HTMLDialogElement>(null);
  useEffect(() => {
    const dialog = panel.current;
    if (!dialog) return;
    const previous = document.activeElement;
    dialog.showModal();
    // Move initial focus to the dialog container itself rather than flashing
    // a focus ring onto the Close button on open.
    dialog.focus({ preventScroll: true });
    return () => {
      dialog.close();
      if (previous instanceof HTMLElement) previous.focus();
    };
  }, []);
  return (
    <dialog
      ref={panel}
      tabIndex={-1}
      className={className ? `wa-dialog ${className}` : "wa-dialog"}
      aria-label={title}
      onCancel={(event) => {
        event.preventDefault();
        onClose();
      }}
      onClick={(event) => {
        if (event.target === event.currentTarget) {
          const rect = event.currentTarget.getBoundingClientRect();
          if (
            event.clientX < rect.left ||
            event.clientX > rect.right ||
            event.clientY < rect.top ||
            event.clientY > rect.bottom
          )
            onClose();
        }
      }}
    >
      <header className="wa-dialog-head">
        <div className="wa-dialog-title-wrap">
          <div className="wa-dialog-title-row">
            <h2>{title}</h2>
            {badge && <span className="chip wa-dialog-badge">{badge}</span>}
          </div>
          {subtitle && <p className="wa-dialog-sub">{subtitle}</p>}
        </div>
        <Button
          className="btn-ghost wa-dialog-close"
          icon={<IconClose />}
          aria-label={t("Close {title}").replace("{title}", () => title)}
          onClick={onClose}
        >
          {t("Close")}
        </Button>
      </header>
      <div className="wa-dialog-body">{children}</div>
      {footer && <footer className="wa-dialog-foot">{footer}</footer>}
    </dialog>
  );
}
