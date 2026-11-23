import type { ReactNode } from "react";
import DrawerShell from "../app-shell/shared/DrawerShell";

/** The one drawer a Settings row opens: details, forms and destructive actions. */
export default function SettingsDrawer({
  title,
  onClose,
  children,
}: {
  title: string;
  onClose: () => void;
  children: ReactNode;
}) {
  return (
    <DrawerShell
      kind="settings-item"
      scrim="board"
      modal
      trapFocus
      size="md"
      label={title}
      title={title}
      onClose={onClose}
    >
      <div className="grid gap-3 min-w-0">{children}</div>
    </DrawerShell>
  );
}
