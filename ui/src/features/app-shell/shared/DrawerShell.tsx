import {
  useCallback,
  useEffect,
  useLayoutEffect,
  useRef,
  useState,
  type CSSProperties,
  type KeyboardEvent,
  type PointerEvent as ReactPointerEvent,
  type ReactNode,
  type RefObject,
} from "react";
import { createPortal } from "react-dom";
import Button from "../../../ui/Button";
import { IconClose } from "../../../ui/icons";
import { useTranslate } from "../../../lib/localeText";
import { useDrawerClose } from "./useDrawerClose";

/**
 * Reusable detail-drawer shell across native board surfaces and `@cadence/screen-ui`.
 *  - header: optional avatar/icon, kicker, title, subtitle, status pills/tags,
 *    optional warning strip (text only, never a button), and gliding tabs;
 *  - body: `children` (single view), active tab panel, or `edit.body` swapped in place;
 *  - footer: pinned. Actions group on the left (⋯ menu, secondary, primary) and
 *    read-only note on the right. While editing it becomes Cancel / Save.
 * `scrim="outlet"` dims the outlet only so the chat pane stays usable.
 * `scrim="board"` covers the full document (`sm` 24rem, `md` 34rem default,
 * `lg` 42rem) and turns into a bottom sheet with grab-drag dismissal at <= 860px.
 * Supports LIFO stacking (`data-depth`, parent `data-receded` + `inert`),
 * guarded dismissal (`preventClose` + micro-nudge), scroll-edge shadows,
 * `portal`, `initialFocus`, and Escape/Tab focus containment.
 */
export type DrawerTab = { id: string; label: string; badge?: ReactNode; panel: ReactNode };
export type DrawerSize = "sm" | "md" | "lg";
export type DrawerMenuItem = {
  key: string;
  label: string;
  onSelect: () => void;
  destructive?: boolean;
  disabled?: boolean;
  title?: string;
};
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
  /** Small accent line above the title (board scrim). */
  kicker?: ReactNode;
  /** `board` dims the whole board, not only the outlet. Default `outlet`. */
  scrim?: "outlet" | "board";
  /** Width token for the drawer (`sm` 24rem, `md` 34rem, `lg` 42rem). */
  size?: DrawerSize;
  /** The host traps focus inside the drawer (CAD-2186); announce it as modal. */
  modal?: boolean;
  /** Own keyboard containment and dismissal for this modal, including nested drawers. */
  trapFocus?: boolean;
  /** Prevent dismissal on scrim/Escape/close/drag (e.g. while a submit is busy). */
  preventClose?: boolean;
  /** Optional status note surfaced when dismissal is blocked by `preventClose`. */
  preventCloseNote?: ReactNode;
  /** Optional target to focus when the drawer mounts (falls back to heading). */
  initialFocus?: RefObject<HTMLElement | null>;
  /** Render scrim and drawer into `document.body`. Defaults to `true` for `scrim="board"` and `false` for `scrim="outlet"`. */
  portal?: boolean;
  /** Optional extra class name appended to the drawer panel. */
  className?: string;
  /** Optional extra class name appended to the scrim element. */
  scrimClassName?: string;
  /** Override accessible label for the top close button. */
  closeLabel?: string;
  /** Extra `data-*` attributes forwarded onto the dialog panel. */
  dataAttributes?: Record<`data-${string}`, string | undefined>;
  title: ReactNode;
  subtitle?: ReactNode;
  avatar?: ReactNode;
  pills?: ReactNode;
  /** Plain warning copy; rendered as a strip with no controls. */
  warning?: ReactNode;
  /** Single-view body when `tabs` are not provided. */
  children?: ReactNode;
  tabs?: DrawerTab[];
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
  /** Replaces the footer's menu / note / secondary / primary slots. */
  foot?: ReactNode;
  onClose: () => void;
  /** Frame-local scrim edge. When supplied, no host outlet is measured. */
  geometry?: { left: number };
}

type StackEntry = {
  id: symbol;
  closing: boolean;
  notify: () => void;
};

const drawerStack: StackEntry[] = [];

function notifyDrawerStack() {
  for (const entry of drawerStack) entry.notify();
}

function getStackState(id: symbol): { isTopmost: boolean; depth: number; receded: boolean } {
  const index = drawerStack.findIndex((entry) => entry.id === id);
  if (index === -1) return { isTopmost: true, depth: 0, receded: false };
  const isTopmost = index === drawerStack.length - 1;
  const receded = drawerStack.slice(index + 1).some((entry) => !entry.closing);
  return { isTopmost, depth: index, receded };
}

function listFocusable(root: HTMLElement): HTMLElement[] {
  const checkLayout = typeof root.getClientRects === "function" && root.getClientRects().length > 0;
  return Array.from(
    root.querySelectorAll<HTMLElement>(
      'button:not([disabled]):not([tabindex="-1"]), input:not([disabled]):not([tabindex="-1"]), select:not([disabled]):not([tabindex="-1"]), textarea:not([disabled]):not([tabindex="-1"]), a[href]:not([tabindex="-1"]), [tabindex]:not([tabindex="-1"])',
    ),
  ).filter((element) => {
    if (element.closest('[role="dialog"]') !== root) return false;
    if (element.closest("[hidden], [inert]") !== null) return false;
    if (checkLayout && element.getClientRects().length === 0) return false;
    return true;
  });
}

/** Left edge of the outlet, so the scrim never dims the chat pane. */
function useScrimLeft(explicitLeft: number | undefined): number {
  const [left, setLeft] = useState(0);
  useLayoutEffect(() => {
    if (explicitLeft !== undefined) return;
    const measure = () => {
      const outlet = document.querySelector(".app-shell-outlet");
      // Below 1024px the chat is a closed sheet, so the scrim spans all.
      setLeft(outlet !== null && window.innerWidth >= 1024 ? Math.round(outlet.getBoundingClientRect().left) : 0);
    };
    measure();
    window.addEventListener("resize", measure);
    return () => window.removeEventListener("resize", measure);
  }, [explicitLeft]);
  return explicitLeft ?? left;
}

function OverflowMenu({ items, open, setOpen }: { items: DrawerMenuItem[]; open: boolean; setOpen: (v: boolean) => void }) {
  const t = useTranslate();
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
    <div className="s-drawer-menu crm-drawer-menu" ref={wrap} onKeyDown={onKey}>
      <button
        ref={trigger}
        type="button"
        className="btn btn-sm btn-icon"
        aria-label={t("More actions")}
        aria-haspopup="menu"
        aria-expanded={open}
        onClick={() => setOpen(!open)}
      >
        ⋯
      </button>
      {open && (
        <div className="s-drawer-pop crm-drawer-pop" role="menu" aria-label={t("More actions")}>
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
  const {
    kind,
    label,
    kicker,
    scrim = "outlet",
    size,
    preventClose = false,
    preventCloseNote,
    initialFocus,
    portal,
    className,
    scrimClassName,
    closeLabel,
    dataAttributes,
    title,
    subtitle,
    avatar,
    pills,
    warning,
    children,
    tabs = [],
    tab: tabProp,
    onTab,
    edit,
    state,
    menu,
    secondary,
    primary,
    note,
    foot,
    onClose,
  } = props;
  const panelRef = useRef<HTMLDivElement | null>(null);
  const bodyRef = useRef<HTMLDivElement | null>(null);
  const tabsRef = useRef<HTMLDivElement | null>(null);
  const headRef = useRef<HTMLHeadingElement | null>(null);
  const initialFocusRef = useRef(initialFocus);
  initialFocusRef.current = initialFocus;
  const [tabState, setTabState] = useState(tabs[0]?.id ?? "");
  const tab = tabProp ?? tabState;
  const setTab = (id: string) => {
    setTabState(id);
    onTab?.(id);
  };
  const [menuOpen, setMenuOpen] = useState(false);
  const [nudged, setNudged] = useState(false);
  const [guardShown, setGuardShown] = useState(false);
  const nudgeTimer = useRef<number | null>(null);
  const [scrolledTop, setScrolledTop] = useState(false);
  const [scrolledBottom, setScrolledBottom] = useState(false);
  const [inkStyle, setInkStyle] = useState<{ x: number; w: number; top: number; ready: boolean }>({
    x: 0,
    w: 0,
    top: 0,
    ready: false,
  });
  const [dragY, setDragY] = useState(0);
  const dragRef = useRef<{ active: boolean; id: number; startY: number; lastY: number; lastAt: number; velocity: number }>({
    active: false,
    id: -1,
    startY: 0,
    lastY: 0,
    lastAt: 0,
    velocity: 0,
  });

  const { closing, requestClose, onTransitionEnd } = useDrawerClose(onClose);
  const t = useTranslate();
  const scrimLeft = useScrimLeft(props.geometry?.left);
  const editing = edit != null;
  const board = scrim === "board";
  const usePortal = portal ?? board;

  const stackId = useRef<symbol>(Symbol("drawer"));
  const [, setStackTick] = useState(0);
  useLayoutEffect(() => {
    const id = stackId.current;
    const entry: StackEntry = {
      id,
      closing: false,
      notify: () => setStackTick((n) => n + 1),
    };
    drawerStack.push(entry);
    notifyDrawerStack();
    return () => {
      const idx = drawerStack.indexOf(entry);
      if (idx !== -1) drawerStack.splice(idx, 1);
      notifyDrawerStack();
    };
  }, []);

  useLayoutEffect(() => {
    const entry = drawerStack.find((item) => item.id === stackId.current);
    if (!entry || entry.closing === closing) return;
    entry.closing = closing;
    notifyDrawerStack();
  }, [closing]);

  const { isTopmost, depth, receded } = getStackState(stackId.current);

  useLayoutEffect(() => {
    const panel = panelRef.current;
    if (!panel) return;
    if (receded) {
      panel.setAttribute("inert", "");
      return () => panel.removeAttribute("inert");
    }
    panel.removeAttribute("inert");
    return undefined;
  }, [receded]);

  useEffect(() => {
    if (!preventClose) setGuardShown(false);
  }, [preventClose]);

  useEffect(() => {
    return () => {
      if (nudgeTimer.current !== null) window.clearTimeout(nudgeTimer.current);
    };
  }, []);

  const triggerNudge = useCallback(() => {
    if (preventCloseNote !== undefined) setGuardShown(true);
    setNudged(true);
    if (nudgeTimer.current !== null) window.clearTimeout(nudgeTimer.current);
    nudgeTimer.current = window.setTimeout(() => {
      nudgeTimer.current = null;
      setNudged(false);
    }, 260);
  }, [preventCloseNote]);

  const attemptClose = useCallback(() => {
    if (preventClose || edit?.pending) {
      triggerNudge();
      return;
    }
    requestClose();
  }, [preventClose, edit?.pending, triggerNudge, requestClose]);

  // Land on `initialFocus` (when supplied) or the heading so deep links and forms land sensibly.
  useEffect(() => {
    const preferred = initialFocusRef.current?.current;
    if (preferred && preferred.isConnected) preferred.focus();
    else headRef.current?.focus();
  }, []);

  // Bubble-phase window listener lets inner controls (Select, OverflowMenu,
  // row menus, ConfirmDialog) handle Escape first and still catches Escape/Tab
  // even when focus moved to `document.body` after a submit button disabled.
  useEffect(() => {
    const onKeyDown = (event: globalThis.KeyboardEvent) => {
      if (!isTopmost || closing || event.defaultPrevented) return;
      const root = panelRef.current;
      const target = event.target instanceof HTMLElement ? event.target : null;
      // Another higher modal overlay (ConfirmDialog or native <dialog open>) owns its keys.
      if (document.querySelector(".s-confirm-wrap, .crm-confirm-wrap, dialog[open]") !== null) return;
      const targetDialog = target?.closest('[role="dialog"]') ?? null;
      if (targetDialog !== null && root !== null && targetDialog !== root) return;

      if (event.key === "Escape") {
        if (menuOpen) {
          if (props.trapFocus) {
            event.preventDefault();
            event.stopPropagation();
          }
          setMenuOpen(false);
        } else if (edit != null) {
          if (props.trapFocus) {
            event.preventDefault();
            event.stopPropagation();
          }
          if (edit.pending || preventClose) triggerNudge();
          else edit.onCancel();
        } else {
          if (props.trapFocus) {
            event.preventDefault();
            event.stopPropagation();
          }
          attemptClose();
        }
        return;
      }

      if (!props.trapFocus || event.key !== "Tab" || !root) return;
      event.stopPropagation();
      const items = listFocusable(root);
      const active = document.activeElement instanceof HTMLElement ? document.activeElement : null;
      const controls = target?.getAttribute("aria-controls") ?? null;
      const index = items.findIndex(
        (element) =>
          element === active ||
          (controls !== null && element.getAttribute("role") === "combobox" && element.getAttribute("aria-controls") === controls),
      );
      if (items.length === 0) {
        event.preventDefault();
        headRef.current?.focus();
        return;
      }
      const onHeading = active === headRef.current;
      const outside = !(active instanceof Node) || !root.contains(active);
      if (onHeading) {
        if (event.shiftKey) {
          event.preventDefault();
          items[items.length - 1]?.focus();
        } else {
          event.preventDefault();
          items[0]?.focus();
        }
        return;
      }
      if (outside || index === -1 || (event.shiftKey ? index === 0 : index === items.length - 1)) {
        event.preventDefault();
        const next =
          outside || index === -1
            ? event.shiftKey
              ? items.length - 1
              : 0
            : (index + (event.shiftKey ? -1 : 1) + items.length) % items.length;
        items[next]?.focus();
      }
    };

    window.addEventListener("keydown", onKeyDown);
    return () => window.removeEventListener("keydown", onKeyDown);
  }, [isTopmost, closing, props.trapFocus, menuOpen, edit, preventClose, triggerNudge, attemptClose]);

  const active = tabs.find((item) => item.id === tab) ?? tabs[0];
  const showingTabs = state == null && !editing && tabs.length > 1;
  const activeTabId = active?.id ?? "";

  // Scroll-edge shadows.
  const measureScroll = useCallback(() => {
    const body = bodyRef.current;
    if (!body) return;
    setScrolledTop(body.scrollTop > 4);
    setScrolledBottom(body.scrollTop + body.clientHeight < body.scrollHeight - 4);
  }, []);

  useLayoutEffect(() => {
    measureScroll();
    const body = bodyRef.current;
    if (!body) return;
    if (typeof ResizeObserver !== "function") return;
    const observer = new ResizeObserver(measureScroll);
    observer.observe(body);
    return () => observer.disconnect();
  }, [measureScroll, activeTabId, editing, state, children]);

  // Gliding tab underline indicator.
  useLayoutEffect(() => {
    if (!showingTabs) {
      setInkStyle((prev) => (prev.ready ? { x: 0, w: 0, top: 0, ready: false } : prev));
      return;
    }
    const updateInk = () => {
      const strip = tabsRef.current;
      if (!strip) return;
      const selected = strip.querySelector<HTMLElement>('[role="tab"][aria-selected="true"]');
      if (!selected || selected.offsetWidth === 0) {
        setInkStyle((prev) => (prev.ready ? { x: 0, w: 0, top: 0, ready: false } : prev));
        return;
      }
      setInkStyle({
        x: selected.offsetLeft,
        w: selected.offsetWidth,
        top: selected.offsetTop + selected.offsetHeight - 2,
        ready: true,
      });
    };
    updateInk();
    const strip = tabsRef.current;
    if (!strip || typeof ResizeObserver !== "function") return;
    const observer = new ResizeObserver(updateInk);
    observer.observe(strip);
    return () => observer.disconnect();
  }, [showingTabs, activeTabId, tabs.length]);

  const onTabKey = (e: KeyboardEvent, index: number) => {
    if (e.key !== "ArrowRight" && e.key !== "ArrowLeft") return;
    const next = tabs[(index + (e.key === "ArrowRight" ? 1 : tabs.length - 1)) % tabs.length];
    if (!next) return;
    setTab(next.id);
    document.getElementById(`crm-tab-${kind}-${next.id}`)?.focus();
  };

  // Mobile sheet drag-to-dismiss gesture on grab handle.
  const onGrabDown = (event: ReactPointerEvent<HTMLDivElement>) => {
    if (closing) return;
    dragRef.current = {
      active: true,
      id: event.pointerId,
      startY: event.clientY,
      lastY: event.clientY,
      lastAt: event.timeStamp,
      velocity: 0,
    };
    event.currentTarget.setPointerCapture?.(event.pointerId);
  };
  const onGrabMove = (event: ReactPointerEvent<HTMLDivElement>) => {
    const drag = dragRef.current;
    if (!drag.active || drag.id !== event.pointerId) return;
    const dy = Math.max(0, event.clientY - drag.startY);
    const dt = Math.max(1, event.timeStamp - drag.lastAt);
    drag.velocity = (event.clientY - drag.lastY) / dt;
    drag.lastY = event.clientY;
    drag.lastAt = event.timeStamp;
    setDragY(preventClose || edit?.pending ? Math.round(dy * 0.18) : Math.round(dy));
  };
  const endGrabDrag = (event: ReactPointerEvent<HTMLDivElement>, cancelled: boolean) => {
    const drag = dragRef.current;
    if (!drag.active || drag.id !== event.pointerId) return;
    drag.active = false;
    event.currentTarget.releasePointerCapture?.(event.pointerId);
    const currentDragY = dragY;
    setDragY(0);
    if (cancelled) return;
    if (preventClose || edit?.pending) {
      if (currentDragY > 6) triggerNudge();
      return;
    }
    const panelHeight = panelRef.current?.offsetHeight || 600;
    if (currentDragY > Math.max(96, panelHeight * 0.28) || (currentDragY > 24 && drag.velocity > 0.55)) {
      requestClose();
    }
  };

  const resolvedNote = guardShown && preventCloseNote !== undefined ? preventCloseNote : note;
  const hasFooter =
    state == null &&
    (editing || foot != null || (menu?.length ?? 0) > 0 || secondary != null || primary != null || resolvedNote != null);
  const bodyContent = state ?? (editing ? edit.body : tabs.length > 0 ? active?.panel : children);
  const panelStyle: CSSProperties | undefined =
    dragY > 0 ? { transform: `translateY(${dragY}px)`, transition: "none" } : undefined;
  const effectiveSize = size ?? (board ? "md" : undefined);

  const tree = (
    <>
      <div
        className={scrimClassName ? `s-drawer-scrim crm-drawer-scrim ${scrimClassName}` : "s-drawer-scrim crm-drawer-scrim"}
        style={{ left: board ? 0 : scrimLeft }}
        data-scrim={board ? "board" : undefined}
        data-depth={depth > 0 ? String(depth) : undefined}
        data-closing={closing || undefined}
        onClick={attemptClose}
        aria-hidden="true"
      />
      <div
        ref={panelRef}
        {...(dataAttributes ?? {})}
        className={className ? `s-drawer crm-drawer crm-drawer-shell ${className}` : "s-drawer crm-drawer crm-drawer-shell"}
        role="dialog"
        aria-modal={props.modal === true ? "true" : "false"}
        aria-label={label}
        data-drawer={kind}
        data-scrim={board ? "board" : undefined}
        data-size={effectiveSize}
        data-mode={editing ? "edit" : "view"}
        data-depth={depth > 0 ? String(depth) : undefined}
        data-receded={receded ? "true" : undefined}
        data-nudged={nudged ? "true" : undefined}
        data-scrolled-top={scrolledTop ? "true" : undefined}
        data-scrolled-bottom={scrolledBottom ? "true" : undefined}
        data-closing={closing || undefined}
        style={panelStyle}
        onTransitionEnd={onTransitionEnd}
      >
        {board && (
          <div
            className="s-drawer-grab"
            aria-hidden="true"
            onPointerDown={onGrabDown}
            onPointerMove={onGrabMove}
            onPointerUp={(e) => endGrabDrag(e, false)}
            onPointerCancel={(e) => endGrabDrag(e, true)}
          >
            <span className="s-drawer-grab-pill" />
          </div>
        )}
        <header className="s-drawer-top crm-drawer-top">
          <div className="s-drawer-id crm-drawer-id">
            {avatar != null && (
              <span className="s-drawer-avatar crm-drawer-avatar" aria-hidden="true">
                {avatar}
              </span>
            )}
            <div className="s-drawer-titles crm-drawer-titles">
              {kicker != null && <div className="s-drawer-kicker crm-drawer-kicker">{kicker}</div>}
              <h3 ref={headRef} className="s-drawer-title crm-drawer-title" tabIndex={-1}>
                {title}
              </h3>
              {subtitle != null && <div className="s-drawer-sub crm-drawer-sub">{subtitle}</div>}
              {pills != null && <div className="s-drawer-pills crm-drawer-pills">{pills}</div>}
            </div>
            <button
              type="button"
              className="s-drawer-close btn btn-sm btn-ghost btn-icon"
              onClick={attemptClose}
              aria-label={closeLabel ?? `${t("Close")} ${label.toLowerCase()}`}
            >
              <IconClose size={15} />
            </button>
          </div>
          {warning != null && !editing && state == null && (
            <p className="s-drawer-warn crm-drawer-warn" data-testid="drawer-warning">
              {warning}
            </p>
          )}
          {showingTabs && (
            <div
              ref={tabsRef}
              className="s-drawer-tabs crm-drawer-tabs"
              role="tablist"
              aria-label={`${label} ${t("sections")}`}
              data-ink-ready={inkStyle.ready ? "true" : undefined}
            >
              {tabs.map((tabItem, i) => (
                <button
                  key={tabItem.id}
                  id={`crm-tab-${kind}-${tabItem.id}`}
                  type="button"
                  role="tab"
                  className="s-drawer-tab crm-drawer-tab"
                  aria-selected={active?.id === tabItem.id}
                  aria-controls={`crm-panel-${kind}`}
                  tabIndex={active?.id === tabItem.id ? 0 : -1}
                  onClick={() => setTab(tabItem.id)}
                  onKeyDown={(e) => onTabKey(e, i)}
                >
                  {tabItem.label}
                  {tabItem.badge}
                </button>
              ))}
              {inkStyle.ready && (
                <span
                  className="s-drawer-tab-ink"
                  aria-hidden="true"
                  style={{
                    transform: `translateX(${inkStyle.x}px)`,
                    width: `${inkStyle.w}px`,
                    top: `${inkStyle.top}px`,
                  }}
                />
              )}
            </div>
          )}
          {editing && <h4 className="s-drawer-edit-title crm-drawer-edit-title">{edit.title}</h4>}
        </header>
        <div
          ref={bodyRef}
          className="s-drawer-body crm-drawer-body"
          id={`crm-panel-${kind}`}
          role={showingTabs ? "tabpanel" : undefined}
          aria-labelledby={showingTabs && active ? `crm-tab-${kind}-${active.id}` : undefined}
          onScroll={measureScroll}
        >
          {bodyContent}
        </div>
        {hasFooter && (
          <footer
            className="s-drawer-foot crm-drawer-foot"
            data-state={resolvedNote != null && !editing ? "read-only" : undefined}
          >
            {editing ? (
              <>
                <div className="s-drawer-actions crm-drawer-actions">
                  <Button size="sm" variant="ghost" onClick={edit.onCancel} disabled={edit.pending}>
                    {t("Cancel")}
                  </Button>
                  <button
                    type="submit"
                    form={edit.formId}
                    className="btn btn-sm btn-primary"
                    disabled={edit.pending}
                    aria-busy={edit.pending || undefined}
                  >
                    {edit.pending ? t("Saving…") : (edit.saveLabel ?? t("Save"))}
                  </button>
                </div>
                {resolvedNote != null && <span className="s-drawer-note crm-drawer-note">{resolvedNote}</span>}
              </>
            ) : foot != null ? (
              foot
            ) : (
              <>
                <div className="s-drawer-actions crm-drawer-actions">
                  {menu != null && menu.length > 0 && (
                    <OverflowMenu items={menu} open={menuOpen} setOpen={setMenuOpen} />
                  )}
                  {secondary}
                  {primary}
                </div>
                {resolvedNote != null && (
                  <span
                    className="s-drawer-note crm-drawer-note"
                    data-tone={guardShown ? "warn" : undefined}
                    role={guardShown ? "status" : undefined}
                  >
                    {resolvedNote}
                  </span>
                )}
              </>
            )}
          </footer>
        )}
      </div>
    </>
  );

  if (usePortal && typeof document !== "undefined" && document.body) {
    return createPortal(tree, document.body);
  }
  return tree;
}
