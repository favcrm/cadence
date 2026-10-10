import { useEffect, useId, useRef, useState, type FocusEvent, type KeyboardEvent, type ReactNode } from "react";
import { closeSession } from "../features/auth/session";
import { setThemePref, type ThemePref } from "../lib/theme";
import type { Meta } from "../lib/types";
import { useLocale } from "../lib/locale";
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
 * sign out. The "avatar" trigger is a 24px initial (header, phones); the
 * "row" trigger is the desktop sidebar footer (CAD-1033, CAD-1312): a
 * compact single line — avatar, name, small role and an up-chevron in a
 * `.navlink`-family row.
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
  const [theme, setTheme] = useState<ThemePref>(appliedTheme);
  const [languageBusy, setLanguageBusy] = useState(false);
  const [languageError, setLanguageError] = useState(false);
  const [languageSaved, setLanguageSaved] = useState(false);
  const { userLocale, organizationLocale, isHostedUser, canSetOrganizationLocale, t, setPreference } = useLocale();
  const rootRef = useRef<HTMLDivElement>(null);
  const panelRef = useRef<HTMLDivElement>(null);
  const triggerRef = useRef<HTMLButtonElement>(null);
  const user = meta?.session?.user;
  const name = user?.name || user?.email || user?.handle || user?.sub || "operator";
  const saveLocale = async (scope: "user" | "organization", value: "en" | "zh-TW" | null) => {
    setLanguageBusy(true);
    setLanguageError(false);
    setLanguageSaved(false);
    try {
      await setPreference(scope, value);
      setLanguageSaved(true);
    } catch {
      setLanguageError(true);
    } finally {
      setLanguageBusy(false);
    }
  };

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
  const role = user?.role || "operator";
  return (
    <div ref={rootRef} className={avatar ? "header-control-wrap shrink-0" : "relative shrink-0"} data-account-menu onKeyDown={onKeyDown} onBlur={onBlur}>
      <button
        ref={triggerRef}
        type="button"
        aria-haspopup="dialog"
        aria-expanded={open}
        aria-controls={open ? panelId : undefined}
        aria-label={`${t("Account menu, signed in as")} ${name}`}
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
            <span className="flex min-w-0 flex-1 items-baseline gap-1.5 text-left" title={`${name} · ${role}`}>
              <span className="truncate text-label font-medium text-ink-200">{name}</span>
              <span className="shrink-0 text-micro text-ink-500">{role}</span>
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
          className={`absolute z-50 card p-1.5 shadow-xl ${placement === "below-end" ? "right-0 top-full mt-2 w-64" : "left-0 bottom-full mb-2 w-[max(100%,16rem)]"}`}
        >
          <div className="px-2 py-1.5" title={meta.session ? `session ${meta.session.id}` : undefined}>
            <p className="text-secondary text-ink-100 break-words">{name}</p>
            {user?.email && user.email !== name && <p className="text-micro text-ink-500 break-all">{user.email}</p>}
            {user?.role && <span className="chip bg-ink-800 text-ink-400 mt-1">{user.role}</span>}
          </div>
          {mayWrite && canWrite && (
            <p className="px-2 py-1.5 text-micro text-ink-500">
              {t("Writes commit to the tracker as")} <span className="num text-ink-300 break-all">{actor}</span>
            </p>
          )}
          <div className="my-1 border-t border-ink-700" />
          <div className="flex items-center justify-between gap-2 px-2 py-1.5">
            <span className="text-label text-ink-400">{t("Theme")}</span>
            <div role="group" aria-label={t("Theme")} className="flex gap-0.5 rounded-md bg-ink-800 p-0.5">
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
                  {icon}{t(label)}
                </button>
              ))}
            </div>
          </div>
          {isHostedUser && (
            <div className="px-2 py-2">
              <label htmlFor={`${panelId}-locale`} className="mb-1 block text-label text-ink-400">{t("Language")}</label>
              <select
                id={`${panelId}-locale`}
                value={userLocale ?? "follow"}
                disabled={languageBusy}
                onChange={(event) => void saveLocale("user", event.target.value === "follow" ? null : event.target.value as "en" | "zh-TW")}
                className="w-full rounded border border-ink-700 bg-ink-900 px-2 py-1.5 text-label text-ink-200"
              >
                <option value="follow">{t("Follow organization")}{organizationLocale ? ` (${organizationLocale === "zh-TW" ? "繁體中文" : "English"})` : ""}</option>
                <option value="en">English</option>
                <option value="zh-TW">繁體中文</option>
              </select>
              {canSetOrganizationLocale && (
                <>
                  <label htmlFor={`${panelId}-org-locale`} className="mb-1 mt-2 block text-label text-ink-400">{t("Organization default")}</label>
                  <select
                    id={`${panelId}-org-locale`}
                    value={organizationLocale ?? ""}
                    disabled={languageBusy}
                    onChange={(event) => void saveLocale("organization", event.target.value as "en" | "zh-TW")}
                    className="w-full rounded border border-ink-700 bg-ink-900 px-2 py-1.5 text-label text-ink-200"
                  >
                    <option value="" disabled>{t("Not set")}</option>
                    <option value="en">English</option>
                    <option value="zh-TW">繁體中文</option>
                  </select>
                </>
              )}
              {languageBusy && <p role="status" className="mt-1 text-micro text-ink-400">{t("Saving…")}</p>}
              {languageSaved && <p role="status" className="mt-1 text-micro text-ink-400">{t("Language preference saved.")}</p>}
              {languageError && <p role="alert" className="mt-1 text-micro text-red-400">{t("Could not save language preference.")}</p>}
            </div>
          )}
          {settingsHref && (
            <Link href={settingsHref} onClick={() => setOpen(false)} className="block rounded px-2 py-1.5 text-label text-ink-200 hover:bg-ink-800">
              {t("Settings")}
            </Link>
          )}
          {canWrite && (<button type="button" disabled={busy} onClick={signOut} className="block w-full rounded px-2 py-1.5 text-left text-label text-ink-200 hover:bg-ink-800 disabled:opacity-60">
            {t("Sign out")}
          </button>)}
          {footer}
        </div>
      )}
    </div>
  );
}
