import { useId, useState, type ReactNode } from "react";
import { nextThemePref, setThemePref, type ThemePref } from "../lib/theme";
import { IconMoon, IconSun, IconTheme } from "./icons";

const icons: Record<ThemePref, ReactNode> = {
  system: <IconTheme />,
  light: <IconSun />,
  dark: <IconMoon />,
};

/** Header button cycling system → light → dark; the pick persists per browser. */
export default function ThemeToggle() {
  const tooltipId = useId();
  // What is applied (initTheme ran before the first render), so a pick
  // that could not be stored still shows correctly.
  const [pref, setPref] = useState<ThemePref>(() => {
    const applied = document.documentElement.getAttribute("data-theme");
    return applied === "light" || applied === "dark" ? applied : "system";
  });
  const next = nextThemePref(pref);
  return (
    <span className="header-control-wrap">
      <button
        onClick={() => {
          setThemePref(next);
          setPref(next);
        }}
        className="header-icon text-ink-400"
        aria-label={`theme: ${pref}, switch to ${next}`}
        aria-describedby={tooltipId}
      >
        {icons[pref]}
      </button>
      <span id={tooltipId} role="tooltip" className="header-tooltip">Theme: {pref} · Switch to {next}</span>
    </span>
  );
}
