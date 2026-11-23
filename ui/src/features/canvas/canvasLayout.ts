/**
 * CAD-2635: the canvas arrangement. A pure module: which open views sit in
 * panes (side by side, stacked, a grid of up to four), where the dividers are,
 * and the rules that move a view between a tab and a pane. `canvasStore` holds
 * one `PaneLayout` per conversation and calls these; nothing here touches the
 * DOM or the store.
 *
 * Vocabulary: a *tab* is any open canvas view (canvasStore). A *pane* is a tab
 * that is on stage; the rest stay in the tab strip. `panes` holds tab ids in
 * reading order and may name tabs that are not open yet (a restored
 * arrangement waits for them): every reader goes through `livePanes`.
 */

export type LayoutMode = "tabs" | "split" | "stack" | "grid";
/** What the view_ref card may ask for (`placement`, additive, `v` stays 1). */
export type Placement = "tab" | "beside" | "below";
/** Where a dragged tab was dropped on the stage. */
export type DropZone = "left" | "right" | "bottom" | "replace";

export interface PaneLayout {
  mode: LayoutMode;
  /** Tab ids in reading order. */
  panes: string[];
  /** The first column's share of the stage, 0.2–0.8. */
  cols: number;
  /** The first row's share of the stage, 0.2–0.8. */
  rows: number;
}

export const MAX_PANES = 4;
/** The canvas must be at least this wide (px) to show panes; narrower falls back to tabs. */
export const MULTI_MIN_WIDTH = 900;
export const MIN_SHARE = 0.2;
export const TABS_LAYOUT: PaneLayout = { mode: "tabs", panes: [], cols: 0.5, rows: 0.5 };

const PLACEMENTS: readonly Placement[] = ["tab", "beside", "below"];
/** A known placement, else undefined: an unknown value is ignored, never refused. */
export function placementOf(value: unknown): Placement | undefined {
  return PLACEMENTS.find((p) => p === value);
}

/** The panes that are open tabs, in order; fewer than two is no arrangement. */
export function livePanes(layout: PaneLayout, tabIds: readonly string[]): string[] {
  const live = layout.panes.filter((id, i) => tabIds.includes(id) && layout.panes.indexOf(id) === i).slice(0, MAX_PANES);
  return live.length >= 2 ? live : [];
}

/** The mode the live panes are shown in: two keep the chosen split or stack, three or four are a grid. */
export function modeFor(count: number, preferred: LayoutMode): LayoutMode {
  if (count < 2) return "tabs";
  if (count > 2) return "grid";
  return preferred === "stack" ? "stack" : "split";
}

function make(layout: PaneLayout, panes: string[], preferred: LayoutMode): PaneLayout {
  return panes.length < 2 ? { ...layout, mode: "tabs", panes: [] } : { ...layout, mode: modeFor(panes.length, preferred), panes };
}

/** The layout as it shows: `mode` and `panes` after the tabs that are open are applied. */
export function effective(layout: PaneLayout, tabIds: readonly string[]): { mode: LayoutMode; panes: string[] } {
  const panes = livePanes(layout, tabIds);
  return { mode: modeFor(panes.length, layout.mode), panes };
}

export interface Placed {
  layout: PaneLayout;
  /** False when nothing changed: four panes are showing, or there is no view to anchor to. */
  placed: boolean;
}

/**
 * Put `id` in a pane next to `anchor` (after it, or before it for `before`).
 * `orient` decides split or stack only when the second pane arrives; a third
 * and fourth make it a grid. At four panes a new view is not placed: the
 * caller leaves it as a background tab. A view already in a pane moves.
 */
export function addPane(layout: PaneLayout, tabIds: readonly string[], id: string, anchor: string | null, orient: "split" | "stack", before = false): Placed {
  const live = livePanes(layout, tabIds);
  if (!tabIds.includes(id) || anchor === null || anchor === id || !tabIds.includes(anchor)) return { layout, placed: false };
  const base = live.length === 0 ? [anchor] : live.filter((p) => p !== id);
  if (live.length > 0 && !live.includes(anchor)) return { layout, placed: false };
  if (base.length >= MAX_PANES) return { layout, placed: false };
  const at = base.indexOf(anchor) + (before ? 0 : 1);
  const panes = [...base.slice(0, at), id, ...base.slice(at)];
  return { layout: make(layout, panes, live.length < 2 ? orient : layout.mode), placed: true };
}

/** A dropped tab: left/right are "beside" (before/after the anchor), bottom is "below", replace swaps the anchor out. */
export function dropPane(layout: PaneLayout, tabIds: readonly string[], id: string, anchor: string | null, zone: DropZone): Placed {
  if (zone === "replace") return replacePane(layout, tabIds, anchor, id);
  return addPane(layout, tabIds, id, anchor, zone === "bottom" ? "stack" : "split", zone === "left");
}

/** `id` takes `target`'s place; `target` returns to a tab. With no arrangement, nothing to replace. */
export function replacePane(layout: PaneLayout, tabIds: readonly string[], target: string | null, id: string): Placed {
  const live = livePanes(layout, tabIds);
  if (target === null || target === id || !live.includes(target) || !tabIds.includes(id)) return { layout, placed: false };
  const panes = live.includes(id) ? live.map((p) => (p === target ? id : p === id ? target : p)) : live.map((p) => (p === target ? id : p));
  return { layout: { ...layout, panes }, placed: true };
}

/** The view goes back to a tab; fewer than two panes left is the tab layout. */
export function removePane(layout: PaneLayout, tabIds: readonly string[], id: string): PaneLayout {
  return make(layout, livePanes(layout, tabIds).filter((p) => p !== id), layout.mode);
}

/**
 * The layout switcher. Tabs clears the panes. Side by side and Stacked keep
 * the focused view and the next one; Grid keeps up to four. The focused view
 * leads, then the arrangement's own order, then the rest of the strip.
 */
export function chooseMode(layout: PaneLayout, tabIds: readonly string[], focused: string | null, mode: LayoutMode): PaneLayout {
  if (mode === "tabs") return layout.mode === "tabs" && layout.panes.length === 0 ? layout : { ...layout, mode: "tabs", panes: [] };
  if (tabIds.length < 2) return layout;
  const live = livePanes(layout, tabIds);
  const lead = focused !== null && tabIds.includes(focused) ? focused : tabIds[0];
  const order = [lead, ...live.filter((p) => p !== lead), ...tabIds.filter((t) => t !== lead && !live.includes(t))];
  const count = mode === "grid" ? Math.min(MAX_PANES, tabIds.length) : 2;
  // Keep the arrangement's own order for the panes already on stage.
  const picked = order.slice(0, count);
  const panes = live.length >= 2 ? [...live.filter((p) => picked.includes(p)), ...picked.filter((p) => !live.includes(p))] : picked;
  return make({ ...layout, mode }, panes, mode);
}

export function clampShare(share: number): number {
  return Math.round(Math.min(1 - MIN_SHARE, Math.max(MIN_SHARE, share)) * 1000) / 1000;
}

/** A divider moved by `deltaPx` in a stage `sizePx` long (keyboard steps use the canvas grip's 24px, 96px with Shift). */
export function nudgeShare(share: number, deltaPx: number, sizePx: number): number {
  return sizePx > 0 ? clampShare(share + deltaPx / sizePx) : share;
}

export interface Cell {
  /** Grid lines in the stage's 3×3 template (the middle track is the divider). */
  col: [number, number];
  row: [number, number];
}

/** Where pane `index` of `count` sits. Three panes: two above and one across the bottom. */
export function cellFor(mode: LayoutMode, count: number, index: number): Cell {
  if (mode === "split") return { col: index === 0 ? [1, 2] : [3, 4], row: [1, 4] };
  if (mode === "stack") return { col: [1, 4], row: index === 0 ? [1, 2] : [3, 4] };
  if (mode === "grid") {
    const wide = count === 3 && index === 2;
    return { col: wide ? [1, 4] : index % 2 === 0 ? [1, 2] : [3, 4], row: index < 2 ? [1, 2] : [3, 4] };
  }
  return { col: [1, 4], row: [1, 4] };
}

export interface Dividers {
  /** Between the columns; its rows (the lower pane of three spans both columns). */
  vertical: [number, number] | null;
  horizontal: boolean;
}

export function dividersFor(mode: LayoutMode, count: number): Dividers {
  if (mode === "split") return { vertical: [1, 4], horizontal: false };
  if (mode === "stack") return { vertical: null, horizontal: true };
  if (mode === "grid") return { vertical: count === 3 ? [1, 2] : [1, 4], horizontal: true };
  return { vertical: null, horizontal: false };
}

// ---- persistence: board-local, per conversation, ids and numbers only ----

const KEY_PREFIX = "cadence.canvas.layout.";
const MAX_ID = 300;

function num(value: unknown, fallback: number): number {
  return typeof value === "number" && Number.isFinite(value) ? clampShare(value) : fallback;
}

/** The saved arrangement of a conversation, or the tab layout. Anything malformed is the tab layout. */
export function readLayout(conversation: string | null): PaneLayout {
  if (conversation === null) return TABS_LAYOUT;
  try {
    const raw = globalThis.localStorage?.getItem(KEY_PREFIX + conversation);
    if (!raw) return TABS_LAYOUT;
    const saved = JSON.parse(raw) as Record<string, unknown>;
    const ids = Array.isArray(saved.panes) ? saved.panes.filter((p): p is string => typeof p === "string" && p.length > 0 && p.length <= MAX_ID) : [];
    const panes = ids.filter((p, i) => ids.indexOf(p) === i).slice(0, MAX_PANES);
    const mode = (["split", "stack", "grid"] as const).find((m) => m === saved.mode);
    if (panes.length < 2 || !mode) return TABS_LAYOUT;
    return { mode, panes, cols: num(saved.cols, 0.5), rows: num(saved.rows, 0.5) };
  } catch {
    return TABS_LAYOUT;
  }
}

export function writeLayout(conversation: string | null, layout: PaneLayout): void {
  if (conversation === null) return;
  try {
    if (layout.mode === "tabs") globalThis.localStorage?.removeItem(KEY_PREFIX + conversation);
    else globalThis.localStorage?.setItem(KEY_PREFIX + conversation, JSON.stringify({ mode: layout.mode, panes: layout.panes, cols: layout.cols, rows: layout.rows }));
  } catch {
    // Blocked storage: the arrangement just is not remembered.
  }
}

/** A confirmed identity change: what was saved belongs to the person who was signed in. */
export function clearSavedLayouts(): void {
  try {
    const storage = globalThis.localStorage;
    if (!storage) return;
    const keys: string[] = [];
    for (let i = 0; i < storage.length; i++) {
      const key = storage.key(i);
      if (key?.startsWith(KEY_PREFIX)) keys.push(key);
    }
    for (const key of keys) storage.removeItem(key);
  } catch {
    // Nothing to clear.
  }
}
