import { useEffect, useId, useRef, useState, type FocusEvent, type KeyboardEvent, type ReactNode } from "react";
import { closeSession } from "../features/auth/session";
import type { Meta } from "../lib/types";
import { useLocale } from "../lib/locale";
import { IconChevron } from "./icons";
import Link from "./Link";

/** The shared account menu for the desktop sidebar and mobile header. */
export default function AccountMenu({
  meta,
  actor,
  mayWrite,
  onChange,
  trigger,
  placement,
  settingsHref,
  footer,
  onOpenChange,
}: {
  meta: Meta | null;
  actor: string;
  /** This client may write — shows the "writes commit as" line. */
  mayWrite: boolean;
  onChange: () => void;
  trigger: "avatar" | "row";
  placement: "below-end" | "above-start";
  settingsHref?: string;
  /** Slot at the end of the panel for the version line (CAD-1034). */
  footer?: ReactNode;
  /** Reports the dialog's open state (the sidebar raises itself while it is open). */
  onOpenChange?: (open: boolean) => void;
}) {
  const panelId = useId();
  const [open, setOpen] = useState(false);
  const [busy, setBusy] = useState(false);
  const signingOut = useRef(false);
  const { locale, t } = useLocale();
  const rootRef = useRef<HTMLDivElement>(null);
  const panelRef = useRef<HTMLDivElement>(null);
  const triggerRef = useRef<HTMLButtonElement>(null);
  const user = meta?.session?.user;
  const name = user?.name || user?.email || user?.handle || user?.sub || "operator";

  useEffect(() => {
    if (!open) return;
    onOpenChange?.(true);
    return () => onOpenChange?.(false);
  }, [open]);

  useEffect(() => {
    if (!open) return;
    const closeOutside = (event: PointerEvent) => {
      if (event.target instanceof Node && !rootRef.current?.contains(event.target)) setOpen(false);
    };
    document.addEventListener("pointerdown", closeOutside);
    return () => document.removeEventListener("pointerdown", closeOutside);
  }, [open]);

  if (!meta || meta.signed_in !== true) return null;

  const close = () => {
    setOpen(false);
    triggerRef.current?.focus();
  };
  const focusables = () => Array.from(panelRef.current?.querySelectorAll<HTMLElement>("button:not(:disabled), a[href]") ?? []);
  // Tab past the last control: focus left both the panel and the trigger. A null
  // relatedTarget (window blur, a click on inert space) is left to pointerdown.
  const onBlur = (event: FocusEvent) => {
    const next = event.relatedTarget;
    if (open && next instanceof Node && !rootRef.current?.contains(next)) setOpen(false);
  };
  const onKeyDown = (event: KeyboardEvent) => {
    if (event.key === "Escape" && open) {
      event.stopPropagation();
      close();
    } else if (open && (event.key === "ArrowDown" || event.key === "ArrowUp")) {
      const items = focusables();
      if (items.length === 0) return;
      event.preventDefault();
      const at = items.indexOf(document.activeElement as HTMLElement);
      const step = event.key === "ArrowDown" ? 1 : -1;
      items[(at + step + items.length) % items.length].focus();
    }
  };
  const signOut = () => {
    if (signingOut.current) return;
    signingOut.current = true;
    setBusy(true);
    closeSession()
      .then(() => onChange())
      .catch(() => undefined)
      .finally(() => {
        signingOut.current = false;
        setBusy(false);
      });
  };
  // Read-only blocks workspace writes, not ending your own session.
  const canWrite = !meta.read_only;
  const avatar = trigger === "avatar";
  const role = user?.role || "operator";
  const roleLabel = locale !== "zh-TW" ? role : role === "operator" ? t("Operator role") : role === "member" ? t("Member role") : role;
  // CAD-1555: a read-only board says so next to the role, never just "operator".
  const accessLabel = meta.read_only ? `${roleLabel} · ${t("Read only")}` : roleLabel;
  return (
    <div ref={rootRef} className={avatar ? "header-control-wrap shrink-0" : "relative shrink-0"} data-account-menu onKeyDown={onKeyDown} onBlur={onBlur}>
      <button
        ref={triggerRef}
        type="button"
        aria-haspopup="dialog"
        aria-expanded={open}
        aria-controls={open ? panelId : undefined}
        aria-label={`${t("Account menu, signed in as")} ${name}${meta.read_only ? ` · ${t("Read only")}` : ""}`}
        onClick={() => setOpen((value) => !value)}
        className={avatar
          ? "header-icon"
          : "navlink account-row"}
      >
        <span aria-hidden className="grid h-6 w-6 shrink-0 place-items-center rounded-full bg-accent/15 text-micro font-medium uppercase text-accent">
          {name.trim().charAt(0)}
        </span>
        {!avatar && (
          <>
            <span className="flex min-w-0 flex-1 flex-col items-start text-left" title={`${name} · ${accessLabel}`}>
              <span className="w-full truncate text-secondary font-medium text-ink-100">{name}</span>
              <span className="w-full truncate text-micro text-ink-500">{accessLabel}</span>
            </span>
            <span aria-hidden className="shrink-0 rotate-180 text-ink-500"><IconChevron /></span>
          </>
        )}
      </button>
      {open && (
        <div
          id={panelId}
          ref={panelRef}
          role="dialog"
          aria-label={t("Account")}
          // Any link in the panel (Settings, a footer slot item) navigates away: close.
          onClick={(event) => { if ((event.target as Element).closest("a")) setOpen(false); }}
          className={`absolute z-50 card p-2 shadow-xl ${placement === "below-end" ? "right-0 top-full mt-2 w-64" : "left-0 bottom-full mb-2 w-[max(100%,16rem)]"}`}
        >
          <div className="flex items-start gap-3 px-2 py-2" title={meta.session ? `${t("session")} ${meta.session.id}` : undefined}>
            <span aria-hidden className="grid h-9 w-9 shrink-0 place-items-center rounded-full bg-accent/15 text-secondary font-medium uppercase text-accent">{name.trim().charAt(0)}</span>
            <div className="min-w-0">
              <p className="text-body font-semibold leading-snug text-ink-100 break-words">{name}</p>
              {user?.email && user.email !== name && <p className="text-micro text-ink-500 break-all">{user.email}</p>}
              {user?.role && <span className="chip bg-ink-800 text-ink-400 mt-1">{accessLabel}</span>}
            </div>
          </div>
          {mayWrite && canWrite && (
            <p className="px-2 py-1.5 text-micro text-ink-500">
              {t("Writes commit to the tracker as")} <span className="num text-ink-300 break-all">{actor}</span>
            </p>
          )}
          <div className="my-1 border-t border-ink-700" />
          {settingsHref && (
            <Link href={settingsHref} onClick={() => setOpen(false)} className="block rounded-md px-2.5 py-2 text-secondary text-ink-200 hover:bg-ink-800">
              {t("Settings")}
            </Link>
          )}
          <button type="button" disabled={busy} onClick={signOut} className="block w-full rounded-md px-2.5 py-2 text-left text-secondary text-ink-200 hover:bg-ink-800 disabled:opacity-60">
            {t("Sign out")}
          </button>
          {footer}
        </div>
      )}
    </div>
  );
}
