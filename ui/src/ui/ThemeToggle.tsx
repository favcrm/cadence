import { useEffect, useId, useState, type ReactNode } from "react";
import { nextThemeFor, setThemePref, type ThemePref } from "../lib/theme";
import { useTranslate } from "../lib/locale";
import { IconMoon, IconSun, IconTheme } from "./icons";

const icons: Record<ThemePref, ReactNode> = {
  system: <IconTheme size={14} />,
  light: <IconSun size={14} />,
  dark: <IconMoon size={14} />,
};

/** Header button cycling system → light → dark; the pick persists per browser. */
export default function ThemeToggle() {
  const t = useTranslate();
  const tooltipId = useId();
  // What is applied (initTheme ran before the first render), so a pick
  // that could not be stored still shows correctly.
  const [pref, setPref] = useState<ThemePref>(() => {
    const applied = document.documentElement.getAttribute("data-theme");
    return applied === "light" || applied === "dark" ? applied : "system";
  });
  // Track the OS scheme: a change between render and the click must not leave
  // a stale "next" that renders the theme that is already on screen.
  const [systemDark, setSystemDark] = useState(
    () => typeof matchMedia === "function" && matchMedia("(prefers-color-scheme: dark)").matches,
  );
  useEffect(() => {
    if (typeof matchMedia !== "function") return;
    const query = matchMedia("(prefers-color-scheme: dark)");
    const update = () => setSystemDark(query.matches);
    update();
    query.addEventListener?.("change", update);
    return () => query.removeEventListener?.("change", update);
  }, []);
  const next = nextThemeFor(pref, systemDark);
  const label = t(pref === "system" ? "System" : pref === "light" ? "Light" : "Dark").toLowerCase();
  const nextLabel = t(next === "system" ? "System" : next === "light" ? "Light" : "Dark").toLowerCase();
  const themeLabel = t("Theme:");
  const switchLabel = t("Switch to");
  return (
    <span className="header-control-wrap">
      <button
        onClick={() => {
          setThemePref(next);
          setPref(next);
        }}
        className="header-icon text-ink-400"
        aria-label={`${themeLabel.toLowerCase()} ${label}, ${switchLabel.toLowerCase()} ${nextLabel}`}
        aria-describedby={tooltipId}
      >
        {icons[pref]}
      </button>
      <span id={tooltipId} role="tooltip" className="header-tooltip">{t("Theme:")} {label} · {t("Switch to")} {nextLabel}</span>
    </span>
  );
}
