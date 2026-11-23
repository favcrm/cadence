import { Fragment } from "react";
import { useTranslate } from "../../lib/locale";
import type { MentionCandidate } from "./mentions";
import "./mentions.css";

/**
 * Shared `@mention` autocomplete popover for Composers and comment inputs (CAD-2711).
 * Renders active deliverable participants and read-only unavailable hints separately.
 */
export default function MentionPicker({
  candidates,
  highlighted,
  onHighlight,
  onSelect,
  ariaLabel = "Mention someone",
  placement = "above",
}: {
  candidates: readonly MentionCandidate[];
  /** Index into `candidates` of the currently highlighted active candidate, or `-1`. */
  highlighted: number;
  onHighlight: (index: number) => void;
  onSelect: (candidate: MentionCandidate) => void;
  ariaLabel?: string;
  placement?: "above" | "below";
}) {
  const t = useTranslate();
  if (candidates.length === 0) return null;

  const hasBothGroups = candidates.some((c) => c.active) && candidates.some((c) => !c.active);
  let lastActive: boolean | null = null;

  return (
    <ul
      className="mention-picker"
      data-placement={placement}
      role="listbox"
      aria-label={t(ariaLabel)}
    >
      {candidates.map((c, index) => {
        const showHeader = lastActive === null || lastActive !== c.active;
        lastActive = c.active;
        const active = c.active && index === highlighted;
        const headerLabel = c.active
          ? hasBothGroups
            ? t("In this channel")
            : t("Members & agents")
          : t("Not in channel");
        const initial = c.label.trim().charAt(0).toUpperCase() || "?";
        return (
          <Fragment key={`${c.kind}:${c.id}`}>
            {showHeader && (hasBothGroups || !c.active) && (
              <li role="presentation" className="mention-picker-group">
                {headerLabel}
              </li>
            )}
            <li
              role="option"
              className="mention-picker-opt"
              aria-selected={active}
              aria-disabled={!c.active || undefined}
            >
              <button
                type="button"
                className={`mention-picker-row${active ? " on" : ""}`}
                disabled={!c.active}
                onMouseDown={(e) => {
                  // Keep focus inside the textarea while picking with the pointer.
                  e.preventDefault();
                }}
                onMouseEnter={() => {
                  if (c.active) onHighlight(index);
                }}
                onClick={() => {
                  if (c.active) onSelect(c);
                }}
              >
                <span className="mention-picker-badge" data-kind={c.kind} aria-hidden="true">
                  {c.kind === "agent" ? "@" : initial}
                </span>
                <span className="mention-picker-main">
                  <span className="mention-picker-name">{c.label}</span>
                  <span className="mention-picker-handle">
                    @{c.handle}
                    {c.subtitle ? ` · ${c.subtitle}` : ""}
                  </span>
                </span>
                {c.active ? (
                  <span className="mention-picker-kind">
                    {t(c.kind === "agent" ? "Agent" : "Teammate")}
                  </span>
                ) : (
                  <span className="mention-picker-reason">
                    {t(c.unavailableReason ?? "Not in this channel")}
                  </span>
                )}
              </button>
            </li>
          </Fragment>
        );
      })}
    </ul>
  );
}
