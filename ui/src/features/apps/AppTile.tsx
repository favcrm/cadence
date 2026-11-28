import { useId, type ReactNode } from "react";
import Link from "../../ui/Link";
import { AppGlyph } from "../explorer/shared";
import "./apps.css";

/** One gallery tile. The caller owns its destination, status and sibling actions. */
export function AppTile({ name, icon, title, summary, version, status, statusAction, statusId: suppliedStatusId, tone = "", href, category, meta, note, pinned, compact, actions, onOpen, onPrefetch }: {
  name: string;
  icon?: string;
  title: string;
  summary?: string;
  version: string;
  status: string;
  statusAction?: ReactNode;
  statusId?: string;
  tone?: "" | "info" | "warn" | "muted";
  href: string | null;
  category?: string;
  meta?: ReactNode;
  note?: string;
  pinned?: boolean;
  compact?: boolean;
  actions?: ReactNode;
  onOpen?: () => void;
  onPrefetch?: () => void;
}) {
  const id = useId();
  const nameId = `${id}-name`;
  const statusId = suppliedStatusId ?? `${id}-status`;
  const body = <>
    <div className="app-tile-icon">
      <AppGlyph name={name} icon={icon} title={title} size={compact ? "sm" : "xl"} />
    </div>
    <div className="app-card-copy">
      <div className="app-card-title-group">
        <h3 className="app-card-name" id={nameId}>{href === null ? title : <Link className="app-card-link" href={href} aria-describedby={statusId} onClick={onOpen} onMouseEnter={onPrefetch} onFocus={onPrefetch}>{title}</Link>}</h3>
        {category && <span className="card-category">{category}</span>}
      </div>
      {summary && <p className="app-card-summary">{summary}</p>}
      {meta}
      <div className="card-footer">
        <span className="num">{version}</span>
        {statusAction ?? <span className="app-status" id={statusId} data-state={tone}>{status}</span>}
      </div>
    </div>
  </>;
  return <article className={`card app-card${pinned ? " is-pinned" : ""}`}>
    <div className="app-card-body">{body}{href === null && note && <div className="app-card-copy-note">{note}</div>}</div>
    {actions && <div className="app-card-actions">{actions}</div>}
  </article>;
}

export function AppTileSkeletons({ label, compact = false }: { label: string; compact?: boolean }) {
  return <div className="app-gallery-loading">
    <p className="sr-only" role="status">{label}</p>
    <div className={`apps-grid${compact ? " is-list" : ""}`} aria-hidden="true">
      {Array.from({ length: 5 }, (_, index) => <div key={index} className="card app-card app-card-skeleton">
        <div className="app-card-body">
          <div className="app-tile-icon app-skeleton" />
          <div className="app-card-copy">
            <span className="app-skeleton app-skeleton-title" />
            <div className="app-card-summary"><span className="app-skeleton" /><span className="app-skeleton" /></div>
            <div className="card-footer"><span className="app-skeleton" /></div>
          </div>
        </div>
      </div>)}
    </div>
  </div>;
}
