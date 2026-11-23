/**
 * CAD-2657: the one header row of an app view in the canvas. Left: a way back
 * (the phone sheet, or an expanded canvas) or the app's mark, the view's title
 * (the tab strip already says it when there are two or more) and the view's
 * params as chips. Right: the read-only lock, "Open in <App>", expand, close.
 * Width is the pane's own (a container query in canvas.css): the chips fold
 * into the first one and "+N" and "Open in" goes icon-only, then the actions
 * fold into "⋯". Chips never shrink: they show whole or fold, and the title is
 * what truncates. Every part keeps its full text in a title.
 */
import { useRef, type ReactNode } from "react";
import { useTranslate } from "../../lib/locale";
import { navigate } from "../../lib/useLocation";
import Link from "../../ui/Link";
import RowMenu, { type RowMenuItem } from "../../ui/RowMenu";
import { IconClose, IconLock } from "../../ui/icons";

export interface ViewHeaderProps {
  /** The app's mark; omitted while a back control leads. */
  mark: ReactNode;
  /** The leading back control: closes the phone sheet, or leaves an expanded canvas. */
  back: { onBack: () => void; expanded: boolean } | null;
  /** Null when the tab strip names the view. */
  title: string | null;
  chips: string[];
  /** The read-only note, shown by the lock; null when the viewer has no access. */
  note: string | null;
  /** The "Open in <App>" link; null when the viewer cannot open the app. */
  open: { label: string; href: string; onOpen: () => void } | null;
  /** Null on the phone sheet, which is already the whole screen. */
  expand: { expanded: boolean; enabled: boolean; onToggle: () => void } | null;
  onClose: () => void;
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

export default function ViewHeader({ mark, back, title, chips, note, open, expand, onClose }: ViewHeaderProps) {
  const t = useTranslate();
  const head = useRef<HTMLElement | null>(null);
  const more = chips.slice(1);
  const expandLabel = t(expand?.expanded ? "Collapse canvas" : "Expand canvas");
  const folded: RowMenuItem[] = [];
  if (open) folded.push({ key: "open", label: open.label, hint: note ?? undefined, onSelect: () => { open.onOpen(); navigate(open.href); } });
  if (expand?.enabled) folded.push({ key: "expand", label: expandLabel, onSelect: expand.onToggle });
  return (
    <header ref={head} className="cv-head cv-vhead" data-view-header>
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
      {chips.length === 0 ? null : (
        <span className="cv-chips" data-view-params title={chips.join(" · ")}>
          {chips.map((chip) => <span key={chip} className="cv-chip" title={chip}>{chip}</span>)}
          {more.length === 0 ? null : <span className="cv-chip cv-chip-more" data-chips-more title={more.join(" · ")}>{`+${more.length}`}</span>}
        </span>
      )}
      <span className="cv-gap" />
      {note === null ? null : (
        <span className="cv-lock cv-wide" tabIndex={0} role="img" aria-label={note} data-read-only>
          <IconLock size={14} />
          <span className="cv-tip" role="tooltip">{note}</span>
        </span>
      )}
      {note !== null && open ? <span className="cv-sep cv-wide" aria-hidden /> : null}
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
      {folded.length === 0 ? null : <span className="cv-fold"><RowMenu label={t("More actions")} items={folded} /></span>}
      {back && !back.expanded ? null : (
        <button type="button" className="cv-iconbtn" onClick={onClose} aria-label={t("Close canvas")}>
          <IconClose size={12} />
        </button>
      )}
    </header>
  );
}
