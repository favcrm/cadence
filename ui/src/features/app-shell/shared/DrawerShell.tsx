import { useEffect, useLayoutEffect, useRef, useState, type KeyboardEvent, type ReactNode } from "react";
import Button from "../../../ui/Button";
import { useDrawerClose } from "./useDrawerClose";

/**
 * CAD-1052 one reusable detail-drawer shell for the CRM customer and
 * segment drawers. Layout contract (the clickable mockup is the spec):
 *  - header: avatar/icon, title, subtitle, status pills/tags, an optional
 *    warning strip (text only, never a button) and the tab strip;
 *  - body: the active tab panel, or `edit.body` swapped in place;
 *  - footer: pinned. Every action lives here — the ⋯ menu on the left
 *    (opens upward, destructive item last and red), secondary buttons,
 *    one primary on the right. While editing it becomes Cancel / Save.
 * The scrim dims the outlet only: the left chat pane stays usable.
 * Escape closes an open menu first, then cancels an edit, then the
 * drawer; focus returns through `useDrawerClose`.
 */
export type DrawerTab = { id: string; label: string; panel: ReactNode };
export type DrawerMenuItem = { key: string; label: string; onSelect: () => void; destructive?: boolean; disabled?: boolean; title?: string };
export type DrawerEdit = {
  /** `id` of the `<form>` in `body`; the footer Save submits it. */
  formId: string;
  title: string;
  body: ReactNode;
  saveLabel?: string;
  pending?: boolean;
  onCancel: () => void;
};

export interface DrawerShellProps {
  kind: string;
  label: string;
  title: string;
  subtitle?: ReactNode;
  avatar?: ReactNode;
  pills?: ReactNode;
  /** Plain warning copy; rendered as a strip with no controls. */
  warning?: ReactNode;
  tabs: DrawerTab[];
  /** Optional controlled tab (so a body link can switch tabs). */
  tab?: string;
  onTab?: (id: string) => void;
  /** Replaces the tabs and body while set; footer becomes Cancel / Save. */
  edit?: DrawerEdit | null;
  /** Loading / error content shown instead of the tabs; the footer hides. */
  state?: ReactNode;
  menu?: DrawerMenuItem[];
  secondary?: ReactNode;
  primary?: ReactNode;
  /** Footer text when the viewer has no actions (read-only view). */
  note?: ReactNode;
  onClose: () => void;
}

/** Left edge of the outlet, so the scrim never dims the chat pane. */
function useScrimLeft(): number {
  const [left, setLeft] = useState(0);
  useLayoutEffect(() => {
    const measure = () => {
      const outlet = document.querySelector(".app-shell-outlet");
      // Below 1024px the chat is a closed sheet, so the scrim spans all.
      setLeft(outlet !== null && window.innerWidth >= 1024 ? Math.round(outlet.getBoundingClientRect().left) : 0);
    };
    measure();
    window.addEventListener("resize", measure);
    return () => window.removeEventListener("resize", measure);
  }, []);
  return left;
}

function OverflowMenu({ items, open, setOpen }: { items: DrawerMenuItem[]; open: boolean; setOpen: (v: boolean) => void }) {
  const wrap = useRef<HTMLDivElement | null>(null);
  const trigger = useRef<HTMLButtonElement | null>(null);
  useEffect(() => {
    if (!open) return;
    wrap.current?.querySelector<HTMLElement>('[role="menuitem"]')?.focus();
    const away = (e: MouseEvent) => {
      if (wrap.current && !wrap.current.contains(e.target as Node)) setOpen(false);
    };
    document.addEventListener("mousedown", away);
    return () => document.removeEventListener("mousedown", away);
  }, [open, setOpen]);
  const onKey = (e: KeyboardEvent) => {
    if (e.key !== "ArrowDown" && e.key !== "ArrowUp") return;
    const els = Array.from(wrap.current?.querySelectorAll<HTMLElement>('[role="menuitem"]') ?? []);
    const at = els.indexOf(document.activeElement as HTMLElement);
    const next = e.key === "ArrowDown" ? (at + 1) % els.length : (at - 1 + els.length) % els.length;
    els[next]?.focus();
    e.preventDefault();
  };
  return (
    <div className="crm-drawer-menu" ref={wrap} onKeyDown={onKey}>
      <button
        ref={trigger}
        type="button"
        className="btn btn-sm btn-icon"
        aria-label="More actions"
        aria-haspopup="menu"
        aria-expanded={open}
        onClick={() => setOpen(!open)}
      >
        ⋯
      </button>
      {open && (
        <div className="crm-drawer-pop" role="menu" aria-label="More actions">
          {items.map((item) => (
            <button
              key={item.key}
              type="button"
              role="menuitem"
              className={item.destructive ? "danger" : undefined}
              aria-disabled={item.disabled || undefined}
              title={item.title}
              onClick={() => {
                if (item.disabled) return;
                setOpen(false);
                trigger.current?.focus();
                item.onSelect();
              }}
            >
              {item.label}
            </button>
          ))}
        </div>
      )}
    </div>
  );
}

export default function DrawerShell(props: DrawerShellProps) {
  const { kind, label, title, subtitle, avatar, pills, warning, tabs, tab: tabProp, onTab, edit, state, menu, secondary, primary, note, onClose } = props;
  const headRef = useRef<HTMLHeadingElement | null>(null);
  const [tabState, setTabState] = useState(tabs[0]?.id ?? "");
  const tab = tabProp ?? tabState;
  const setTab = (id: string) => {
    setTabState(id);
    onTab?.(id);
  };
  const [menuOpen, setMenuOpen] = useState(false);
  const { closing, requestClose, onTransitionEnd } = useDrawerClose(onClose);
  const scrimLeft = useScrimLeft();
  const editing = edit != null;

  // Land on the heading so a deep link (no opener) still has a place.
  useEffect(() => {
    headRef.current?.focus();
  }, []);
  useEffect(() => {
    const onKeyDown = (e: globalThis.KeyboardEvent) => {
      if (e.key !== "Escape") return;
      if (menuOpen) setMenuOpen(false);
      else if (edit != null) edit.onCancel();
      else requestClose();
    };
    addEventListener("keydown", onKeyDown);
    return () => removeEventListener("keydown", onKeyDown);
  }, [menuOpen, edit, requestClose]);

  const active = tabs.find((t) => t.id === tab) ?? tabs[0];
  const onTabKey = (e: KeyboardEvent, index: number) => {
    if (e.key !== "ArrowRight" && e.key !== "ArrowLeft") return;
    const next = tabs[(index + (e.key === "ArrowRight" ? 1 : tabs.length - 1)) % tabs.length];
    setTab(next.id);
    document.getElementById(`crm-tab-${kind}-${next.id}`)?.focus();
  };
  const hasFooter = state == null && (editing || (menu?.length ?? 0) > 0 || secondary != null || primary != null || note != null);

  return (
    <>
      <div
        className="crm-drawer-scrim"
        style={{ left: scrimLeft }}
        data-closing={closing || undefined}
        onClick={requestClose}
        aria-hidden="true"
      />
      <div
        className="crm-drawer crm-drawer-shell"
        role="dialog"
        aria-modal="false"
        aria-label={label}
        data-drawer={kind}
        data-mode={editing ? "edit" : "view"}
        data-closing={closing || undefined}
        onTransitionEnd={onTransitionEnd}
      >
        <header className="crm-drawer-top">
          <div className="crm-drawer-id">
            {avatar != null && <span className="crm-drawer-avatar" aria-hidden="true">{avatar}</span>}
            <div className="crm-drawer-titles">
              <h3 ref={headRef} className="crm-drawer-title" tabIndex={-1}>
                {title}
              </h3>
              {subtitle != null && <div className="crm-drawer-sub">{subtitle}</div>}
              {pills != null && <div className="crm-drawer-pills">{pills}</div>}
            </div>
            <button type="button" className="btn btn-sm btn-ghost btn-icon" onClick={requestClose} aria-label={`Close ${label.toLowerCase()}`}>
              ✕
            </button>
          </div>
          {warning != null && !editing && state == null && (
            <p className="crm-drawer-warn" data-testid="drawer-warning">{warning}</p>
          )}
          {state == null && !editing && tabs.length > 1 && (
            <div className="crm-drawer-tabs" role="tablist" aria-label={`${label} sections`}>
              {tabs.map((t, i) => (
                <button
                  key={t.id}
                  id={`crm-tab-${kind}-${t.id}`}
                  type="button"
                  role="tab"
                  className="crm-drawer-tab"
                  aria-selected={active?.id === t.id}
                  aria-controls={`crm-panel-${kind}`}
                  tabIndex={active?.id === t.id ? 0 : -1}
                  onClick={() => setTab(t.id)}
                  onKeyDown={(e) => onTabKey(e, i)}
                >
                  {t.label}
                </button>
              ))}
            </div>
          )}
          {editing && <h4 className="crm-drawer-edit-title">{edit.title}</h4>}
        </header>
        <div
          className="crm-drawer-body"
          id={`crm-panel-${kind}`}
          role={state == null && !editing && tabs.length > 1 ? "tabpanel" : undefined}
          aria-labelledby={state == null && !editing && tabs.length > 1 && active ? `crm-tab-${kind}-${active.id}` : undefined}
        >
          {state ?? (editing ? edit.body : active?.panel)}
        </div>
        {hasFooter && (
          <footer className="crm-drawer-foot" data-state={note != null && !editing ? "read-only" : undefined}>
            {editing ? (
              <>
                <span className="crm-drawer-spacer" />
                <Button size="sm" variant="ghost" onClick={edit.onCancel} disabled={edit.pending}>
                  Cancel
                </Button>
                <button type="submit" form={edit.formId} className="btn btn-sm btn-primary" disabled={edit.pending} aria-busy={edit.pending || undefined}>
                  {edit.pending ? "Saving…" : (edit.saveLabel ?? "Save")}
                </button>
              </>
            ) : (
              <>
                {menu != null && menu.length > 0 && <OverflowMenu items={menu} open={menuOpen} setOpen={setMenuOpen} />}
                {note != null && <span className="crm-drawer-note">{note}</span>}
                <span className="crm-drawer-spacer" />
                {secondary}
                {primary}
              </>
            )}
          </footer>
        )}
      </div>
    </>
  );
}
