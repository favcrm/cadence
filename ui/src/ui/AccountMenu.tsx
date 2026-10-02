import { useEffect, useId, useRef, useState, type FocusEvent, type KeyboardEvent, type ReactNode } from "react";
import { closeSession } from "../features/auth/session";
import { setThemePref, type ThemePref } from "../lib/theme";
import type { Meta } from "../lib/types";
import { IconChevron, IconMoon, IconSun, IconTheme } from "./icons";
import Link from "./Link";

const THEMES: { pref: ThemePref; label: string; icon: ReactNode }[] = [
  { pref: "system", label: "System", icon: <IconTheme size={14} /> },
  { pref: "light", label: "Light", icon: <IconSun size={14} /> },
  { pref: "dark", label: "Dark", icon: <IconMoon size={14} /> },
];

/** The applied theme (initTheme ran before the first render). */
function appliedTheme(): ThemePref {
  const applied = document.documentElement.getAttribute("data-theme");
  return applied === "light" || applied === "dark" ? applied : "system";
}

/**
 * One account menu: who you are, who writes are attributed to, theme and
 * sign out. The "avatar" trigger is a 28px initial (header, phones); the
 * "row" trigger is the desktop sidebar footer (CAD-1033): avatar, name,
 * "role · host" and an up-chevron in a `.navlink`-family row.
 */
export default function AccountMenu({
  meta,
  actor,
  mayWrite,
  onChange,
  trigger,
  placement,
  settingsHref,
  footer,
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
}) {
  const panelId = useId();
  const [open, setOpen] = useState(false);
  const [busy, setBusy] = useState(false);
  const signingOut = useRef(false);
  const [theme, setTheme] = useState<ThemePref>(appliedTheme);
  const rootRef = useRef<HTMLDivElement>(null);
  const panelRef = useRef<HTMLDivElement>(null);
  const triggerRef = useRef<HTMLButtonElement>(null);
  const user = meta?.session?.user;
  const name = user?.name || user?.email || user?.handle || user?.sub || "operator";

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
      .catch(() => undefined)
      .finally(() => {
        signingOut.current = false;
        setBusy(false);
        onChange();
      });
  };
  // A read-only board shows nothing that writes: the logout route skips the read-only guard.
  const canWrite = !meta.read_only;
  const avatar = trigger === "avatar";
  const hostLine = `${user?.role || "operator"} · ${location.host}`;
  return (
    <div ref={rootRef} className={avatar ? "header-control-wrap shrink-0" : "relative shrink-0"} data-account-menu onKeyDown={onKeyDown} onBlur={onBlur}>
      <button
        ref={triggerRef}
        type="button"
        aria-haspopup="dialog"
        aria-expanded={open}
        aria-controls={open ? panelId : undefined}
        aria-label={`Account menu, signed in as ${name}`}
        onClick={() => setOpen((value) => !value)}
        className={avatar
          ? "header-icon"
          : "navlink account-row"}
      >
        <span aria-hidden className="grid h-7 w-7 shrink-0 place-items-center rounded-full bg-accent/15 text-label font-medium uppercase text-accent">
          {name.trim().charAt(0)}
        </span>
        {!avatar && (
          <>
            <span className="min-w-0 flex-1 text-left" title={hostLine}>
              <span className="block truncate text-secondary font-medium text-ink-200">{name}</span>
              <span className="block truncate text-micro text-ink-500">{hostLine}</span>
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
          aria-label="Account"
          className={`absolute z-50 card p-1.5 shadow-xl ${placement === "below-end" ? "right-0 top-full mt-2 w-64" : "left-0 bottom-full mb-2 w-[max(100%,16rem)]"}`}
        >
          <div className="px-2 py-1.5" title={meta.session ? `session ${meta.session.id}` : undefined}>
            <p className="text-secondary text-ink-100 break-words">{name}</p>
            {user?.email && user.email !== name && <p className="text-micro text-ink-500 break-all">{user.email}</p>}
            {user?.role && <span className="chip bg-ink-800 text-ink-400 mt-1">{user.role}</span>}
          </div>
          {mayWrite && canWrite && (
            <p className="px-2 py-1.5 text-micro text-ink-500">
              Writes commit to the tracker as <span className="num text-ink-300 break-all">{actor}</span>
            </p>
          )}
          <div className="my-1 border-t border-ink-700" />
          <div className="flex items-center justify-between gap-2 px-2 py-1.5">
            <span className="text-label text-ink-400">Theme</span>
            <div role="group" aria-label="Theme" className="flex gap-0.5 rounded-md bg-ink-800 p-0.5">
              {THEMES.map(({ pref, label, icon }) => (
                <button
                  key={pref}
                  type="button"
                  aria-pressed={theme === pref}
                  onClick={() => {
                    setThemePref(pref);
                    setTheme(pref);
                  }}
                  className={`inline-flex items-center gap-1 rounded px-1.5 py-1 text-micro ${theme === pref ? "bg-ink-700 text-ink-100" : "text-ink-400 hover:text-ink-200"}`}
                >
                  {icon}{label}
                </button>
              ))}
            </div>
          </div>
          {settingsHref && (
            <Link href={settingsHref} onClick={() => setOpen(false)} className="block rounded px-2 py-1.5 text-label text-ink-200 hover:bg-ink-800">
              Settings
            </Link>
          )}
          {canWrite && (<button type="button" disabled={busy} onClick={signOut} className="block w-full rounded px-2 py-1.5 text-left text-label text-ink-200 hover:bg-ink-800 disabled:opacity-60">
            Sign out
          </button>)}
          {footer}
        </div>
      )}
    </div>
  );
}
