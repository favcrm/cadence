import { useId, useState, type ReactNode } from "react";
import { useLocale } from "../../lib/locale";
import { setThemePref, type ThemePref } from "../../lib/theme";
import { IconMoon, IconSun, IconTheme } from "../../ui/icons";

const THEMES: { pref: ThemePref; label: string; icon: ReactNode }[] = [
  { pref: "system", label: "System", icon: <IconTheme size={14} /> },
  { pref: "light", label: "Light", icon: <IconSun size={14} /> },
  { pref: "dark", label: "Dark", icon: <IconMoon size={14} /> },
];

export default function Preferences() {
  const id = useId();
  const { userLocale, organizationLocale, isHostedUser, canSetOrganizationLocale, t, setPreference } = useLocale();
  const [theme, setTheme] = useState<ThemePref>(() => {
    const applied = document.documentElement.getAttribute("data-theme");
    return applied === "light" || applied === "dark" ? applied : "system";
  });
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState(false);
  const [saved, setSaved] = useState(false);

  const saveLocale = async (scope: "user" | "organization", value: "en" | "zh-TW" | null) => {
    setBusy(true);
    setError(false);
    setSaved(false);
    try {
      await setPreference(scope, value);
      setSaved(true);
    } catch {
      setError(true);
    } finally {
      setBusy(false);
    }
  };

  return (
    <main className="px-4 lg:px-8 py-5 min-w-0 max-w-2xl">
      <h1 className="text-section font-semibold text-ink-100">{t("Preferences")}</h1>
      <section className="mt-6 border-b border-ink-700 pb-6" aria-labelledby={`${id}-theme`}>
        <h2 id={`${id}-theme`} className="text-cardtitle font-medium text-ink-100">{t("Theme")}</h2>
        <div role="group" aria-labelledby={`${id}-theme`} className="mt-3 inline-flex flex-wrap gap-1 rounded-md bg-ink-800 p-1">
          {THEMES.map(({ pref, label, icon }) => (
            <button key={pref} type="button" aria-pressed={theme === pref}
              onClick={() => { setThemePref(pref); setTheme(pref); }}
              className={`inline-flex items-center gap-2 rounded px-3 py-2 text-secondary ${theme === pref ? "bg-ink-850 text-ink-100 shadow-sm" : "text-ink-400 hover:text-ink-100"}`}>
              {icon}{t(label)}
            </button>
          ))}
        </div>
      </section>
      <section className="pt-6" aria-labelledby={`${id}-language-heading`}>
        <h2 id={`${id}-language-heading`} className="text-cardtitle font-medium text-ink-100">{t("Language")}</h2>
        <label htmlFor={`${id}-locale`} className="sr-only">{t("Language")}</label>
        <select id={`${id}-locale`} value={userLocale ?? "follow"} disabled={busy}
          onChange={(event) => void saveLocale("user", event.target.value === "follow" ? null : event.target.value as "en" | "zh-TW")}
          className="mt-3 block w-full max-w-xs rounded border border-ink-700 bg-ink-900 px-3 py-2 text-secondary text-ink-200">
          <option value="follow">{isHostedUser ? `${t("Follow organization")}${organizationLocale ? ` (${organizationLocale === "zh-TW" ? "繁體中文" : "English"})` : ""}` : t("Browser default")}</option>
          <option value="en">{t("English")}</option>
          <option value="zh-TW">繁體中文</option>
        </select>
        {canSetOrganizationLocale && <>
          <label htmlFor={`${id}-org-locale`} className="mt-5 block text-label text-ink-400">{t("Organization default")}</label>
          <select id={`${id}-org-locale`} value={organizationLocale ?? ""} disabled={busy}
            onChange={(event) => void saveLocale("organization", event.target.value as "en" | "zh-TW")}
            className="mt-2 block w-full max-w-xs rounded border border-ink-700 bg-ink-900 px-3 py-2 text-secondary text-ink-200">
            <option value="" disabled>{t("Not set")}</option>
            <option value="en">{t("English")}</option>
            <option value="zh-TW">繁體中文</option>
          </select>
        </>}
        {busy && <p role="status" className="mt-2 text-label text-ink-400">{t("Saving…")}</p>}
        {saved && <p role="status" className="mt-2 text-label text-ink-400">{t(isHostedUser ? "Language preference saved." : "Language saved in this browser.")}</p>}
        {error && <p role="alert" className="mt-2 text-label text-fail">{t("Could not save language preference.")}</p>}
      </section>
    </main>
  );
}
