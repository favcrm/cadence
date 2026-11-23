/**
 * CAD-2635: the compact layout switcher in the canvas tab-strip row: Tabs,
 * Side by side, Stacked, Grid. Where the canvas is too narrow for panes it is
 * disabled, and a "⊞ N panes saved" chip says the arrangement is kept and
 * comes back when the canvas is wide again (a click expands the canvas, when
 * that is what it takes).
 */
import { useTranslate } from "../../lib/locale";
import type { LayoutMode } from "./canvasLayout";

const ICONS: Record<LayoutMode, string> = {
  tabs: "M3 8h18M3 8V6a1 1 0 0 1 1-1h16a1 1 0 0 1 1 1v2M3 8v10a1 1 0 0 0 1 1h16a1 1 0 0 0 1-1V8",
  split: "M4 5h16a1 1 0 0 1 1 1v12a1 1 0 0 1-1 1H4a1 1 0 0 1-1-1V6a1 1 0 0 1 1-1zM12 5v14",
  stack: "M4 5h16a1 1 0 0 1 1 1v12a1 1 0 0 1-1 1H4a1 1 0 0 1-1-1V6a1 1 0 0 1 1-1zM3 12h18",
  grid: "M4 5h16a1 1 0 0 1 1 1v12a1 1 0 0 1-1 1H4a1 1 0 0 1-1-1V6a1 1 0 0 1 1-1zM12 5v14M3 12h18",
};

export default function LayoutSwitcher({ mode, enabled, saved, canExpand, onChoose, onExpand }: {
  /** The layout showing now. */
  mode: LayoutMode;
  /** The canvas is wide enough (or can be expanded to be) for panes. */
  enabled: boolean;
  /** Saved panes the canvas is too narrow to show; 0 when none. */
  saved: number;
  canExpand: boolean;
  onChoose: (mode: LayoutMode) => void;
  onExpand: () => void;
}) {
  const t = useTranslate();
  const options: [LayoutMode, string][] = [
    ["tabs", t("Tabs")],
    ["split", t("Side by side")],
    ["stack", t("Stacked")],
    ["grid", t("Grid")],
  ];
  const chip = t("{n} panes saved").replace("{n}", String(saved));
  return (
    <div className="cv-layout">
      {saved >= 2 ? (
        canExpand ? (
          <button type="button" className="cv-saved" data-panes-saved title={t("Expand the canvas to show the saved panes")} onClick={onExpand}>{`⊞ ${chip}`}</button>
        ) : (
          <span className="cv-saved" data-panes-saved title={t("Panes need a wider canvas")}>{`⊞ ${chip}`}</span>
        )
      ) : null}
      <div className="cv-switch" role="group" aria-label={t("Canvas layout")}>
        {options.map(([value, label]) => (
          <button
            key={value}
            type="button"
            className="cv-switch-btn"
            data-layout-mode={value}
            aria-pressed={mode === value}
            disabled={!enabled}
            title={enabled ? label : `${label} — ${t("Panes need a wider canvas")}`}
            onClick={() => onChoose(value)}
          >
            <svg viewBox="0 0 24 24" width="14" height="14" fill="none" stroke="currentColor" strokeWidth="1.8" strokeLinecap="round" strokeLinejoin="round" aria-hidden><path d={ICONS[value]} /></svg>
            <span className="cv-switch-label">{label}</span>
          </button>
        ))}
      </div>
    </div>
  );
}
