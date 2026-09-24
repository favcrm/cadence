import { useState, type ReactNode } from "react";
import { nextThemePref, setThemePref, type ThemePref } from "../lib/theme";

const icons: Record<ThemePref, ReactNode> = {
  system: (
    <svg width="14" height="14" viewBox="0 0 16 16" fill="none" stroke="currentColor" strokeWidth="1.4">
      <circle cx="8" cy="8" r="5.5" />
      <path d="M8 2.5v11a5.5 5.5 0 0 0 0-11z" fill="currentColor" />
    </svg>
  ),
  light: (
    <svg width="14" height="14" viewBox="0 0 16 16" fill="none" stroke="currentColor" strokeWidth="1.4">
      <circle cx="8" cy="8" r="3" />
      <path d="M8 1.5v1.8M8 12.7v1.8M1.5 8h1.8M12.7 8h1.8M3.4 3.4l1.3 1.3M11.3 11.3l1.3 1.3M12.6 3.4l-1.3 1.3M4.7 11.3l-1.3 1.3" />
    </svg>
  ),
  dark: (
    <svg width="14" height="14" viewBox="0 0 16 16" fill="none" stroke="currentColor" strokeWidth="1.4">
      <path d="M13.5 9.6A5.8 5.8 0 0 1 6.4 2.5a5.8 5.8 0 1 0 7.1 7.1z" />
    </svg>
  ),
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
