import { useId, useState } from "react";
import { useLocale } from "../../lib/locale";
import { setThemePref, type ThemePref } from "../../lib/theme";
import Select from "../../ui/Select";

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
    <main className="px-4 lg:px-8 py-5 min-w-0 max-w-3xl">
      <header>
        <h1 className="text-section font-semibold text-ink-100">{t("Preferences")}</h1>
        <p className="mt-1 text-secondary text-ink-400">{t("Choose how Cadence looks and the language you use.")}</p>
      </header>
      <section className="mt-7 border-t border-ink-700" aria-labelledby={`${id}-appearance`}>
        <div className="grid gap-3 py-5 sm:grid-cols-[minmax(0,1fr)_minmax(12rem,16rem)] sm:items-start sm:gap-8">
          <div>
            <h2 id={`${id}-appearance`} className="text-cardtitle font-medium text-ink-100">{t("Theme")}</h2>
            <p className="mt-1 text-label leading-relaxed text-ink-400">{t("Use your system setting or choose a theme for this browser.")}</p>
          </div>
          <Select id={`${id}-theme`} aria-label={t("Theme")} full value={theme}
            options={[{ value: "system", label: t("System") }, { value: "light", label: t("Light") }, { value: "dark", label: t("Dark") }]}
            onChange={(value) => { const pref = value as ThemePref; setThemePref(pref); setTheme(pref); }} />
        </div>
      </section>
      <section className="border-t border-ink-700" aria-labelledby={`${id}-language-heading`}>
        <div className="grid gap-3 py-5 sm:grid-cols-[minmax(0,1fr)_minmax(12rem,16rem)] sm:items-start sm:gap-8">
          <div>
            <h2 id={`${id}-language-heading`} className="text-cardtitle font-medium text-ink-100">{t("Language")}</h2>
            <p className="mt-1 text-label leading-relaxed text-ink-400">{t(isHostedUser ? "Choose your language or follow the organization default." : "Choose your language or follow your browser setting.")}</p>
          </div>
          <div className="min-w-0">
            <Select id={`${id}-locale`} aria-label={t("Language")} full value={userLocale ?? "follow"} disabled={busy}
              onChange={(value) => void saveLocale("user", value === "follow" ? null : value as "en" | "zh-TW")}
              options={[
                { value: "follow", label: isHostedUser ? `${t("Follow organization")}${organizationLocale ? ` (${organizationLocale === "zh-TW" ? "繁體中文" : "English"})` : ""}` : t("Browser default") },
                { value: "en", label: t("English") },
                { value: "zh-TW", label: "繁體中文" },
              ]} />
            {busy && <p role="status" className="mt-2 text-label text-ink-400">{t("Saving…")}</p>}
            {saved && <p role="status" className="mt-2 text-label text-ink-400">{t(isHostedUser ? "Language preference saved." : "Language saved in this browser.")}</p>}
            {error && <p role="alert" className="mt-2 text-label text-fail">{t("Could not save language preference.")}</p>}
          </div>
        </div>
      </section>
      {canSetOrganizationLocale && <section className="border-t border-ink-700" aria-labelledby={`${id}-org-heading`}>
        <div className="grid gap-3 py-5 sm:grid-cols-[minmax(0,1fr)_minmax(12rem,16rem)] sm:items-start sm:gap-8">
          <div>
            <h2 id={`${id}-org-heading`} className="text-cardtitle font-medium text-ink-100">{t("Organization default")}</h2>
            <p className="mt-1 text-label leading-relaxed text-ink-400">{t("The default language for members who follow the organization.")}</p>
          </div>
          <Select id={`${id}-org-locale`} aria-label={t("Organization default")} full value={organizationLocale ?? ""} disabled={busy}
            placeholder={t("Not set")}
            onChange={(value) => void saveLocale("organization", value as "en" | "zh-TW")}
            options={[{ value: "en", label: t("English") }, { value: "zh-TW", label: "繁體中文" }]} />
        </div>
      </section>}
    </main>
  );
}
