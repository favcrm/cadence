import type { ReactNode } from "react";
import { IconLock } from "./icons";

/**
 * CAD-1312: a screen's one calm full-pane state — a sign-in or access
 * refusal, an empty workspace — instead of a boxed card among sections.
 * Presentational only: the caller decides when it applies and keeps every
 * gate and fetch. `icon` defaults to the lock; `actions` sits under the copy.
 */
export default function PageState({
  title,
  icon = <IconLock size={32} />,
  actions,
  children,
}: {
  title: string;
  icon?: ReactNode;
  actions?: ReactNode;
  children?: ReactNode;
}) {
  return (
    <div className="page-state">
      <span className="page-state-icon" aria-hidden>{icon}</span>
      <h1 className="page-state-title">{title}</h1>
      {children != null && <p className="page-state-copy">{children}</p>}
      {actions}
    </div>
  );
}
