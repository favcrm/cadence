/**
 * CAD-2657 / CAD-2690: an app view's header. Its own row (a way back or the
 * app's mark, the view's title, then "Open in <App>", expand and close) exists
 * only where no tab bar carries them: with a tab bar the actions dock at the
 * right of that row and the mark stays on the tab. Under it, one quiet line:
 * "<App> · <options> · Read-only". Width is the pane's own (a container query in
 * canvas.css): "Open in" goes icon-only, then the actions fold into "⋯". The line
 * truncates with an ellipsis and keeps its full text in a title.
 */
import { useId, useRef, type ReactNode } from "react";
import { createPortal } from "react-dom";
import { useTranslate } from "../../lib/locale";
import { navigate } from "../../lib/useLocation";
import Link from "../../ui/Link";
import RowMenu, { type RowMenuItem } from "../../ui/RowMenu";
import { IconClose } from "../../ui/icons";

export interface ViewHeaderProps {
  /** The app's mark; omitted while a back control leads. */
  mark: ReactNode;
  /** The leading back control: closes the phone sheet, or leaves an expanded canvas. */
  back: { onBack: () => void; expanded: boolean } | null;
  /** Null when the tab strip names the view. */
  title: string | null;
  appName: string;
  /** The view's options in words (viewParams.ts); "" when it has none. */
  options: string;
  /** The read-only note, the "Read-only" word's tooltip; null when the viewer has no access. */
  note: string | null;
  /** The "Open in <App>" link; null when the viewer cannot open the app. */
  open: { label: string; href: string; onOpen: () => void } | null;
  /** Null on the phone sheet, which is already the whole screen. */
  expand: { expanded: boolean; enabled: boolean; onToggle: () => void } | null;
  /** In a pane (CAD-2635): "Move to tab" sits in the "⋯", and the close returns the view to its tab. */
  pane?: { onMoveToTab: () => void } | null;
  /** Whether this view is mounted editable instead of read-only. */
  editing?: boolean;
  /** Operator-gated edit toggle ("Edit here" / "Done editing"); null when unavailable. */
  edit?: { active: boolean; label: string; onToggle: () => void } | null;
  onClose: () => void;
  /** The tab bar's action slot: undefined when no tab bar leads (the actions stay here); null until it mounts. */
  tabRow?: HTMLElement | null;
}

/** Leaving the expanded canvas removes the focused "Back to chat": hand focus to the expand control (or the "⋯" that holds it) rather than the page. */
function focusExpand(root: HTMLElement | null): void {
  requestAnimationFrame(() => {
    for (const target of [root?.querySelector<HTMLElement>("[data-expand]"), root?.querySelector<HTMLElement>(".cv-fold button")]) {
      target?.focus();
      if (target && document.activeElement === target) return;
    }
  });
}

export default function ViewHeader({ mark, back, title, appName, options, note, open, expand, pane = null, editing = false, edit = null, onClose, tabRow }: ViewHeaderProps) {
  const t = useTranslate();
  const head = useRef<HTMLElement | null>(null);
  const tip = useId();
  const docked = tabRow !== undefined && back === null && pane === null;
  const statusLabel = editing ? t("Editing") : note === null ? null : t("Read-only");
  const line = [appName, ...(options === "" ? [] : [options]), ...(statusLabel === null ? [] : [statusLabel])].join(" · ");
  const expandLabel = t(expand?.expanded ? "Collapse canvas" : "Expand canvas");
  const folded: RowMenuItem[] = [];
  if (pane) folded.push({ key: "tab", label: t("Move to tab"), onSelect: pane.onMoveToTab });
  if (edit) folded.push({ key: "edit", label: edit.label, onSelect: edit.onToggle });
  if (open) folded.push({ key: "open", label: open.label, hint: editing ? undefined : (note ?? undefined), onSelect: () => { open.onOpen(); navigate(open.href); } });
  if (expand?.enabled) folded.push({ key: "expand", label: expandLabel, onSelect: expand.onToggle });
  const actions = (
    <>
      {edit ? (
        <button
          type="button"
          className={`btn btn-ghost cv-edit cv-wide${edit.active ? " is-editing" : ""}`}
          onClick={edit.onToggle}
          data-canvas-edit
          aria-pressed={edit.active}
        >
          {edit.label}
        </button>
      ) : null}
      {open ? (
        // Navigating away leaves the tab behind: the caller closes it (the sheet closes the drawer).
        <Link className="btn btn-ghost cv-open cv-wide" href={open.href} title={open.label} aria-label={open.label} onClick={open.onOpen}>
          <svg viewBox="0 0 24 24" width="14" height="14" fill="none" stroke="currentColor" strokeWidth="1.8" strokeLinecap="round" strokeLinejoin="round" aria-hidden>
            <path d="M14 4h6v6M20 4l-9 9M18 14v5a1 1 0 0 1-1 1H5a1 1 0 0 1-1-1V7a1 1 0 0 1 1-1h5" />
          </svg>
          <span className="cv-open-label">{open.label}</span>
        </Link>
      ) : null}
      {expand ? (
        <button type="button" className="cv-iconbtn cv-wide" onClick={expand.onToggle} disabled={!expand.enabled} data-expand aria-pressed={expand.expanded} aria-label={expandLabel} title={expandLabel}>
          <svg viewBox="0 0 24 24" width="14" height="14" fill="none" stroke="currentColor" strokeWidth="1.8" strokeLinecap="round" strokeLinejoin="round" aria-hidden>
            {expand.expanded ? <path d="M9 4v5H4M15 4v5h5M9 20v-5H4M15 20v-5h5" /> : <path d="M4 9V4h5M20 9V4h-5M4 15v5h5M20 15v5h-5" />}
          </svg>
        </button>
      ) : null}
      {folded.length === 0 ? null : <span className="cv-fold" data-pane={pane ? "" : undefined}><RowMenu label={t("More actions")} items={folded} /></span>}
      {back && !back.expanded ? null : (
        <button type="button" className="cv-iconbtn" onClick={onClose} aria-label={t(pane ? "Close pane" : "Close canvas")} title={pane ? t("Close pane — the view stays in the tabs") : undefined}>
          <IconClose size={12} />
        </button>
      )}
    </>
  );
  return (
    <header ref={head} className="cv-head cv-vhead" data-view-header data-docked={docked || undefined}>
      {docked ? (tabRow ? createPortal(actions, tabRow) : null) : (
        <div className="cv-vrow">
          {back ? (
            <>
              <button type="button" className="cv-iconbtn cv-back" data-expanded={back.expanded || undefined} onClick={() => { back.onBack(); if (back.expanded) focusExpand(head.current); }} aria-label={t("Back to chat")}>
                <svg viewBox="0 0 24 24" width="16" height="16" fill="none" stroke="currentColor" strokeWidth="1.8" strokeLinecap="round" strokeLinejoin="round" aria-hidden>
                  <path d="m15 18-6-6 6-6" />
                </svg>
                {back.expanded ? <><span>{t("Back to chat")}</span><kbd>Esc</kbd></> : null}
              </button>
              {back.expanded ? <span className="cv-sep" aria-hidden /> : null}
            </>
          ) : null}
          {back?.expanded ? null : mark}
          {title === null ? null : <b className="cv-vtitle" title={title}>{title}</b>}
          <span className="cv-gap" />
          {actions}
        </div>
      )}
      <p className="cv-meta" data-view-options>
        <span className="cv-meta-text" title={line}>{appName}{options === "" ? null : ` · ${options}`}</span>
        {editing ? (
          <>
            <span className="cv-meta-dot" aria-hidden>·</span>
            <span className="cv-editing" data-canvas-editing>{t("Editing")}</span>
          </>
        ) : note === null ? null : (
          <>
            <span className="cv-meta-dot" aria-hidden>·</span>
            <span className="cv-ro" tabIndex={0} aria-describedby={tip} data-read-only>
              {t("Read-only")}
              <span id={tip} className="cv-tip" role="tooltip">{note}</span>
            </span>
          </>
        )}
      </p>
    </header>
  );
}
