import type { ReactNode } from "react";
import { useTranslate } from "../../src/lib/localeText";
import { Button } from "../../src/ui/Button";

export interface FilterChipOption<Id extends string = string> {
  id: Id;
  label: string;
  count?: number;
  active?: boolean;
  disabled?: boolean;
}

export interface FilterBarProps<ChipId extends string = string> {
  query?: string;
  onQueryChange?: (next: string) => void;
  searchPlaceholder?: string;
  searchLabel?: string;
  chips?: ReadonlyArray<FilterChipOption<ChipId>>;
  chipsLabel?: string;
  onChipToggle?: (id: ChipId) => void;
  actions?: ReactNode;
  onClear?: () => void;
  clearLabel?: string;
  className?: string;
}

/** Search input, toggleable facet chips, and right-aligned actions in one responsive bar. */
export function FilterBar<ChipId extends string = string>({
  query,
  onQueryChange,
  searchPlaceholder,
  searchLabel,
  chips,
  chipsLabel,
  onChipToggle,
  actions,
  onClear,
  clearLabel,
  className,
}: FilterBarProps<ChipId>) {
  const t = useTranslate();
  const resolvedLabel = searchLabel ?? t("Filter");
  const resolvedPlaceholder = searchPlaceholder ?? resolvedLabel;
  const hasActiveFilter = Boolean(
    (query && query.length > 0) || chips?.some((chip) => Boolean(chip.active)),
  );

  return (
    <div
      className={className ? `s-filter-bar ${className}` : "s-filter-bar"}
      role="search"
      aria-label={resolvedLabel}
    >
      {onQueryChange ? (
        <div className="s-filter-bar-search">
          <input
            type="search"
            className="field"
            value={query ?? ""}
            onChange={(event) => onQueryChange(event.target.value)}
            placeholder={resolvedPlaceholder}
            aria-label={resolvedLabel}
          />
        </div>
      ) : null}
      {chips && chips.length > 0 ? (
        <div
          className="s-filter-bar-chips"
          role="group"
          aria-label={chipsLabel ?? t("{label} options").replace("{label}", () => resolvedLabel)}
        >
          {chips.map((chip) => (
            <Button
              key={chip.id}
              size="sm"
              variant={chip.active ? "primary" : "secondary"}
              aria-pressed={Boolean(chip.active)}
              disabled={chip.disabled}
              onClick={() => onChipToggle?.(chip.id)}
            >
              {chip.count !== undefined ? `${chip.label} (${chip.count})` : chip.label}
            </Button>
          ))}
        </div>
      ) : null}
      {(onClear && hasActiveFilter) || actions ? (
        <div className="s-filter-bar-actions">
          {onClear && hasActiveFilter ? (
            <Button size="sm" variant="ghost" onClick={onClear}>
              {clearLabel ?? t("Clear")}
            </Button>
          ) : null}
          {actions}
        </div>
      ) : null}
    </div>
  );
}
