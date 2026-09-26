/** Pure listbox behaviour for the Select primitive. The component is a thin view over this. */

export type SelectOption = {
  value: string;
  label: string;
  hint?: string;
  badge?: string;
  disabled?: boolean;
  group?: string;
};

/** A search field appears on its own once the list is longer than this. */
export const SEARCH_THRESHOLD = 8;

export const TYPEAHEAD_MS = 500;

export type SelectModel = {
  open: boolean;
  /** Index into the filtered list. -1 when nothing is highlighted. */
  active: number;
  query: string;
  buffer: string;
  bufferAt: number;
};

export type KeyMods = { meta?: boolean; ctrl?: boolean; alt?: boolean };

export type KeyResult = {
  model: SelectModel;
  /** A value to commit, or null when the key did not pick. */
  pick: string | null;
  prevent: boolean;
};

export function closedSelect(): SelectModel {
  return { open: false, active: -1, query: "", buffer: "", bufferAt: 0 };
}

export function showsSearch(options: readonly SelectOption[], searchable?: boolean): boolean {
  return searchable === true || options.length > SEARCH_THRESHOLD;
}

export function filterOptions(options: readonly SelectOption[], query: string): SelectOption[] {
  const q = query.trim().toLowerCase();
  if (!q) return options.slice();
  return options.filter((option) =>
    [option.label, option.hint, option.badge, option.value, option.group].some((part) =>
      part?.toLowerCase().includes(q),
    ),
  );
}

/** Badge copy is ordinary text — a cost label, not an icon. */
export function optionBadgeText(option: SelectOption): string | null {
  const badge = option.badge?.trim();
  return badge ? badge : null;
}

export function enabledIndexes(options: readonly SelectOption[]): number[] {
  const indexes: number[] = [];
  options.forEach((option, index) => {
    if (!option.disabled) indexes.push(index);
  });
  return indexes;
}

export function canPick(option: SelectOption | undefined): option is SelectOption {
  return option !== undefined && !option.disabled;
}

export function moveActive(
  options: readonly SelectOption[],
  active: number,
  key: "ArrowDown" | "ArrowUp" | "Home" | "End",
): number {
  const enabled = enabledIndexes(options);
  if (enabled.length === 0) return -1;
  if (key === "Home") return enabled[0] ?? -1;
  if (key === "End") return enabled[enabled.length - 1] ?? -1;
  const pos = enabled.indexOf(active);
  if (key === "ArrowDown") {
    if (pos === -1) return enabled[0] ?? -1;
    return enabled[Math.min(pos + 1, enabled.length - 1)] ?? -1;
  }
  if (pos === -1) return enabled[enabled.length - 1] ?? -1;
  return enabled[Math.max(pos - 1, 0)] ?? -1;
}

/**
 * Typeahead. A single new character starts after `from` and wraps. Extra
 * characters inside the timeout extend the prefix and may stay on `from`.
 * A disabled option is never a match. No match leaves `from` alone.
 */
export function typeaheadIndex(options: readonly SelectOption[], buffer: string, from: number): number {
  const q = buffer.trim().toLowerCase();
  if (!q) return from;
  const enabled = enabledIndexes(options);
  if (enabled.length === 0) return -1;
  const includeCurrent = buffer.length > 1;
  const order = includeCurrent
    ? [...enabled.filter((index) => index >= from), ...enabled.filter((index) => index < from)]
    : [...enabled.filter((index) => index > from), ...enabled.filter((index) => index <= from)];
  for (const index of order) {
    const option = options[index];
    if (!option) continue;
    if (option.label.toLowerCase().startsWith(q) || option.value.toLowerCase().startsWith(q)) return index;
  }
  return from;
}

function activeOnOpen(options: readonly SelectOption[], value: string): number {
  const selected = options.findIndex((option) => option.value === value && !option.disabled);
  if (selected >= 0) return selected;
  return enabledIndexes(options)[0] ?? -1;
}

export function openSelect(options: readonly SelectOption[], value: string): SelectModel {
  return { open: true, active: activeOnOpen(options, value), query: "", buffer: "", bufferAt: 0 };
}

const MOVE_KEYS = ["ArrowDown", "ArrowUp", "Home", "End"] as const;
type MoveKey = (typeof MOVE_KEYS)[number];

function isMoveKey(key: string): key is MoveKey {
  return (MOVE_KEYS as readonly string[]).includes(key);
}

export function reduceSelectKey(
  model: SelectModel,
  options: readonly SelectOption[],
  searchOn: boolean,
  value: string,
  key: string,
  now: number,
  mods: KeyMods = {},
): KeyResult {
  const list = filterOptions(options, searchOn ? model.query : "");

  if (isMoveKey(key)) {
    if (!model.open) {
      // Opening lands on the selection. Home/End jump; Up/Down do not skip past it.
      const selected = list.findIndex((option) => option.value === value && !option.disabled);
      let active: number;
      if (key === "Home") active = moveActive(list, -1, "Home");
      else if (key === "End") active = moveActive(list, -1, "End");
      else if (key === "ArrowUp") active = selected >= 0 ? selected : moveActive(list, -1, "End");
      else active = selected >= 0 ? selected : (enabledIndexes(list)[0] ?? -1);
      return {
        model: { ...model, open: true, active, buffer: "", bufferAt: 0 },
        pick: null,
        prevent: true,
      };
    }
    return {
      model: { ...model, active: moveActive(list, model.active, key) },
      pick: null,
      prevent: true,
    };
  }

  if (key === "Escape") {
    if (!model.open) return { model, pick: null, prevent: false };
    return { model: closedSelect(), pick: null, prevent: true };
  }

  if (key === "Tab") {
    if (!model.open) return { model, pick: null, prevent: false };
    return { model: closedSelect(), pick: null, prevent: false };
  }

  if (key === "Enter" || key === " ") {
    if (!model.open) {
      return { model: openSelect(list, value), pick: null, prevent: true };
    }
    const option = list[model.active];
    if (!canPick(option)) return { model, pick: null, prevent: true };
    return { model: closedSelect(), pick: option.value, prevent: true };
  }

  if (key.length === 1 && !mods.meta && !mods.ctrl && !mods.alt && !searchOn) {
    const fresh = now - model.bufferAt > TYPEAHEAD_MS;
    const buffer = (fresh ? "" : model.buffer) + key;
    const from = model.open && !fresh ? model.active : model.open ? model.active : -1;
    return {
      model: {
        open: true,
        active: typeaheadIndex(list, buffer, from),
        query: model.query,
        buffer,
        bufferAt: now,
      },
      pick: null,
      prevent: true,
    };
  }

  return { model, pick: null, prevent: false };
}

export function reduceQuery(
  model: SelectModel,
  options: readonly SelectOption[],
  query: string,
  value: string,
): SelectModel {
  const previous = filterOptions(options, model.query);
  const highlighted = previous[model.active];
  const list = filterOptions(options, query);
  const highlightedSurvives =
    highlighted !== undefined && list.some((option) => option.value === highlighted.value && !option.disabled);
  const prefer = highlightedSurvives && highlighted ? highlighted.value : value;
  return { ...model, open: true, query, active: activeOnOpen(list, prefer), buffer: "", bufferAt: 0 };
}

export function reduceOutside(model: SelectModel): SelectModel {
  if (!model.open) return model;
  return closedSelect();
}

export function reducePick(model: SelectModel, option: SelectOption | undefined): KeyResult {
  if (!canPick(option)) return { model, pick: null, prevent: true };
  return { model: closedSelect(), pick: option.value, prevent: true };
}

export type Placement = "above" | "below";

/** Flip the popover above the trigger when the viewport bottom cannot hold it. */
export function flipPlacement(
  trigger: { top: number; bottom: number },
  popoverHeight: number,
  viewportHeight: number,
  gap = 4,
): Placement {
  const spaceBelow = viewportHeight - trigger.bottom - gap;
  const spaceAbove = trigger.top - gap;
  if (spaceBelow < popoverHeight && spaceAbove > spaceBelow) return "above";
  return "below";
}
