import { useState } from "react";
import {
  activeCount,
  NO_FILTERS,
  toggle,
  type BoardFilters,
  type Facet,
} from "../filters";
import type { IssueCard } from "../types";

interface Props {
  /** Cards in scope before the chips apply — the facet values and
   *  their counts come from here, so a chip never counts itself out. */
  scope: IssueCard[];
  /** Every issue — epic chips show the epic's title on hover. */
  issues: IssueCard[];
  filters: BoardFilters;
  onChange: (f: BoardFilters) => void;
}

/// value → how many cards in scope carry it, most used first.
function tally(values: (string | undefined)[][]): [string, number][] {
  const counts = new Map<string, number>();
  for (const vs of values) {
    for (const v of vs) if (v) counts.set(v, (counts.get(v) ?? 0) + 1);
  }
  return [...counts].sort((a, b) => b[1] - a[1] || a[0].localeCompare(b[0]));
}

export default function FilterBar({ scope, issues, filters, onChange }: Props) {
  const titleOf = new Map(issues.map((i) => [i.id, i.title]));
  const facets: [Facet, string, [string, number][]][] = [
    ["tags", "tag", tally(scope.map((t) => t.tags ?? []))],
    ["epics", "epic", tally(scope.map((t) => [t.parent]))],
    ["owners", "owner", tally(scope.map((t) => [t.owner]))],
    ["components", "component", tally(scope.map((t) => [t.component]))],
  ];
  const active = activeCount(filters);
  const doneCount = scope.filter((t) => t.status === "done").length;
  // Below `sm` the facet chips fold behind one toggle so the bar never
  // pushes the board off a phone screen.
  const [open, setOpen] = useState(false);

  return (
    <section
      className="card mb-4 px-4 py-2.5 flex flex-wrap items-start gap-x-5 gap-y-2 reveal"
      style={{ animationDelay: "100ms" }}
      aria-label="Filters"
    >
      <button
        aria-expanded={open}
        onClick={() => setOpen(!open)}
        className="chip !py-[.15rem] bg-ink-800 text-ink-400 hover:text-ink-200 transition-colors"
      >
        {open ? "hide filters" : "filters"}{active > 0 ? ` · ${active}` : ""}
      </button>
      <div className={open ? "contents" : "hidden"}>
      {facets.map(([facet, label, options]) => {
        // A value picked through the URL stays removable even when no
        // card in scope carries it any more.
        const missing = filters[facet]
          .filter((v) => !options.some(([o]) => o === v))
          .map((v): [string, number] => [v, 0]);
        const all = [...options, ...missing];
        if (all.length === 0) return null;
        return (
          <div key={facet} className="flex items-center gap-2 min-w-0 flex-wrap">
            <span className="slabel">{label}</span>
            <div className="flex flex-wrap gap-1.5">
              {all.map(([value, n]) => {
                const on = filters[facet].includes(value);
                return (
                  <button
                    key={value}
                    aria-pressed={on}
                    title={facet === "epics" ? titleOf.get(value) : undefined}
                    onClick={() => onChange(toggle(filters, facet, value))}
                    className={`chip !py-[.15rem] transition-colors ${
                      on
                        ? "bg-accent/10 text-accent"
                        : "bg-ink-800 text-ink-400 hover:text-ink-200"
                    }`}
                  >
                    {value}
                    <span className={on ? "text-accent/70" : "text-ink-500"}>
                      {n}
                    </span>
                  </button>
                );
              })}
            </div>
          </div>
        );
      })}
      </div>
      <div className="ml-auto flex items-center gap-1.5">
        {doneCount > 0 && (
          <button
            aria-pressed={filters.showDone}
            onClick={() =>
              onChange({ ...filters, showDone: !filters.showDone })
            }
            className={`chip !py-[.15rem] transition-colors ${
              filters.showDone
                ? "bg-accent/10 text-accent"
                : "bg-ink-800 text-ink-400 hover:text-ink-200"
            }`}
            title="finished issues are hidden by default — a search still matches them"
          >
            done {doneCount}
          </button>
        )}
        <button
          aria-pressed={filters.groupByEpic}
          onClick={() =>
            onChange({ ...filters, groupByEpic: !filters.groupByEpic })
          }
          className={`chip !py-[.15rem] transition-colors ${
            filters.groupByEpic
              ? "bg-accent/10 text-accent"
              : "bg-ink-800 text-ink-400 hover:text-ink-200"
          }`}
          title="one swimlane per epic, with its progress"
        >
          group by epic
        </button>
        {active > 0 && (
          <button
            onClick={() =>
              onChange({
                ...NO_FILTERS,
                groupByEpic: filters.groupByEpic,
                showDone: filters.showDone,
              })
            }
            className="chip !py-[.15rem] bg-ink-800 text-ink-400 hover:text-ink-200 transition-colors"
          >
            clear {active}
          </button>
        )}
      </div>
    </section>
  );
}
