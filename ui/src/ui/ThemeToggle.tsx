import { useState, type ReactNode } from "react";
import { nextThemePref, setThemePref, type ThemePref } from "../lib/theme";
import { IconMoon, IconSun, IconTheme } from "./icons";

const icons: Record<ThemePref, ReactNode> = {
  system: <IconTheme />,
  light: <IconSun />,
  dark: <IconMoon />,
};

/** Header button cycling system → light → dark; the pick persists per browser. */
export default function ThemeToggle() {
  // What is applied (initTheme ran before the first render), so a pick
  // that could not be stored still shows correctly.
  const [pref, setPref] = useState<ThemePref>(() => {
    const applied = document.documentElement.getAttribute("data-theme");
    return applied === "light" || applied === "dark" ? applied : "system";
  });
  const next = nextThemePref(pref);
  return (
    <button
      onClick={() => {
        setThemePref(next);
        setPref(next);
      }}
      className="inline-flex items-center justify-center w-7 h-7 rounded text-ink-400 hover:bg-ink-800 hover:text-ink-100"
      title={`theme: ${pref} — switch to ${next}`}
      aria-label={`theme: ${pref}, switch to ${next}`}
    >
      {icons[pref]}
    </button>
  );
}
