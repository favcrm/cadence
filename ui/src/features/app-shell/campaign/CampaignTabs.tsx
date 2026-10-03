import type { ReactNode } from "react";
import type { CampaignTab } from "./readiness";

export interface TabSpec {
  id: CampaignTab;
  label: string;
  /** A short status pill next to the label (e.g. "1 draft"). */
  badge?: string | null;
}

const ORDER: CampaignTab[] = ["overview", "email", "audience", "activity"];

/**
 * The saved campaign's tablist (CAD-1055). Only the active panel is
 * mounted; Left/Right/Home/End move between tabs (roving tabindex).
 */
export default function CampaignTabs({
  tabs,
  active,
  onChange,
  children,
}: {
  tabs: TabSpec[];
  active: CampaignTab;
  onChange: (tab: CampaignTab) => void;
  children: ReactNode;
}) {
  const move = (from: CampaignTab, key: string) => {
    const index = ORDER.indexOf(from);
    const next =
      key === "ArrowRight" ? (index + 1) % ORDER.length
      : key === "ArrowLeft" ? (index + ORDER.length - 1) % ORDER.length
      : key === "Home" ? 0
      : key === "End" ? ORDER.length - 1
      : -1;
    if (next < 0) return false;
    onChange(ORDER[next]);
    requestAnimationFrame(() =>
      document.getElementById(`cmp-tab-${ORDER[next]}`)?.focus(),
    );
    return true;
  };
  return (
    <>
      <div className="crm-ctabs" role="tablist" aria-label="Campaign sections">
        {tabs.map((tab) => (
          <button
            key={tab.id}
            id={`cmp-tab-${tab.id}`}
            type="button"
            role="tab"
            className="crm-ctab"
            data-tab={tab.id}
            aria-selected={active === tab.id}
            aria-controls={`cmp-panel-${tab.id}`}
            tabIndex={active === tab.id ? 0 : -1}
            onClick={() => onChange(tab.id)}
            onKeyDown={(e) => {
              if (move(tab.id, e.key)) e.preventDefault();
            }}
          >
            {tab.label}
            {tab.badge ? <span className="chip">{tab.badge}</span> : null}
          </button>
        ))}
      </div>
      <div
        id={`cmp-panel-${active}`}
        role="tabpanel"
        aria-labelledby={`cmp-tab-${active}`}
        className="grid gap-3"
        data-panel={active}
      >
        {children}
      </div>
    </>
  );
}
