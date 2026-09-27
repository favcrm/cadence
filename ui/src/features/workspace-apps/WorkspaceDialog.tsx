import { useEffect, useRef, type ReactNode } from "react";
import Button from "../../ui/Button";
import { IconClose } from "../../ui/icons";

export function WorkspaceDialog({
  title,
  onClose,
  children,
}: {
  title: string;
  onClose: () => void;
  children: ReactNode;
}) {
  const panel = useRef<HTMLDialogElement>(null);
  useEffect(() => {
    const dialog = panel.current;
    if (!dialog) return;
    const previous = document.activeElement;
    dialog.showModal();
    return () => {
      dialog.close();
      if (previous instanceof HTMLElement) previous.focus();
    };
  }, []);
  return (
    <dialog
      ref={panel}
      className="wa-dialog"
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
        <h2>{title}</h2>
        <Button
          icon={<IconClose />}
          aria-label={`Close ${title}`}
          onClick={onClose}
        >
          Close
        </Button>
      </header>
      <div className="wa-dialog-body">{children}</div>
    </dialog>
  );
}
