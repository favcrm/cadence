/**
 * CAD-1726: the agent canvas's state. One store for the whole shell: the
 * drawer is mounted once and Home and every app pane feed it through
 * `feedCanvas` (see feed.ts). No React here; `useCanvas` subscribes.
 *
 * Rules (the ticket's):
 * - the drawer opens by itself only for the FIRST artifact of a turn the
 *   operator started, and only when that artifact arrives live, and only on a
 *   host that has not turned auto-open off (`FeedInput.autoOpen`: the member
 *   chat does, so the drawer never covers the chat on a phone), and only
 *   once that turn is no longer the one in flight (CAD-2548: the thread shows
 *   its cards with the answer, so the drawer waits for it too);
 * - a drawer the operator closed stays closed for the rest of that turn;
 * - the drawer closes when the conversation changes;
 * - opening an artifact already open focuses its tab; closing the active
 *   tab selects a neighbour; closing the last collapses the drawer, unless the
 *   Browser tab (browserStore.ts, not modelled here) has a live session or was
 *   opened on its own, and then the drawer stays on it;
 * - an app view (appView.ts, CAD-2471) is a tab that holds a reference to an
 *   installed app's screen. A tap opens it; the feed opens one only for a
 *   live `view_ref` card (CAD-2475) under the same once-per-operator-turn rule
 *   as artifacts, and never adds or moves a tab otherwise. It follows the same
 *   focus, close and conversation-change rules;
 * - a record view (recordView.ts, CAD-2543) is a tab that reads one stored
 *   `detail` record card (an email). It holds the card's own content, never
 *   fetches, and follows the same rules: a tap opens it, the feed opens one
 *   only for a live card under the once-per-operator-turn rule;
 * - the arrangement (canvasLayout.ts, CAD-2635): tabs, or 2-4 views on stage in
 *   panes. `layout` is saved per conversation; a view's pane is its tab, so a
 *   pane's close returns it to the strip and closing the tab closes the view.
 *   A card's `placement` puts a view in a pane next to the focused one
 *   (`beside`, `below`) or leaves it a tab, and an explicit placement is
 *   the one thing the once-per-turn rule lets past.
 */
import { useSyncExternalStore } from "react";
import { onPrivateSessionReset } from "../../lib/privateSession";
import { appViewId, appViewRef, appViewTitle, applyRoutePatch, sameRoute, type AppViewRef } from "./appView";
import type { RoutePatch } from "../workspace-apps/screen/screenRoute";
import type { RecordViewRef } from "./recordView";
import {
  addPane, chooseMode, clampShare, clearSavedLayouts, dropPane, livePanes, readLayout, removePane, replacePane, writeLayout,
  MAX_PANES, type DropZone, type LayoutMode, type PaneLayout, type Placement,
} from "./canvasLayout";

export type ArtifactKind = "md" | "pdf" | "html" | "image" | "csv";
/** A tab's kind: an artifact's, an app view's or a record view's. */
export type TabKind = ArtifactKind | "app" | "record";
export const ARTIFACT_KINDS: readonly ArtifactKind[] = ["md", "pdf", "html", "image", "csv"];

/** What a thread card carries — never the content. */
export interface ArtifactRef {
  id: string;
  version: number;
  kind: ArtifactKind;
  title: string;
  /** Absent when the opener does not know this version's size (CAD-2286: a
   *  document opened at an older version than the one its list row names). */
  bytes?: number;
  digest?: string;
  /** A generated image's shape (`16:9`), shown on its overlay; absent otherwise. */
  aspect?: string;
}

/** What the board knows of one stored version (from its thread card). */
export interface VersionInfo {
  bytes?: number;
  digest?: string;
}

export interface CanvasTab {
  id: string;
  /** `app` is an app view (appView.ts): a reference, with none of the version fields in use. */
  kind: TabKind;
  /** Set exactly for an app view. */
  app?: AppViewRef;
  /** Set exactly for a record view. */
  record?: RecordViewRef;
  title: string;
  /** Absent when the version that opened the tab had no known size. */
  bytes?: number;
  aspect?: string;
  /** The newest version the thread has announced. */
  latest: number;
  /** The version in view. */
  selected: number;
  /** Selected tracks `latest` until the operator picks an older one. */
  follow: boolean;
  /** Sizes and digests of the versions the thread has announced. */
  versions: Record<number, VersionInfo>;
  /** The turn that put the newest version, if known. */
  turn: string | null;
}

export type ArtifactTab = CanvasTab & { kind: ArtifactKind };
export const isArtifactTab = (tab: CanvasTab): tab is ArtifactTab => tab.kind !== "app" && tab.kind !== "record";

/** The turn running in the thread, and what it is changing in the canvas. */
export interface Working {
  turn: string | null;
  /** Artifact ids this turn has asked to add a version to (`--id`). */
  updating: string[];
}

export type Surface = "home" | "app";

export interface CanvasState {
  conversation: string | null;
  /** Which host feeds the canvas (decides how much room the chat keeps). */
  surface: Surface;
  /** The composer draft key of the conversation in view (Ask to change). */
  composerKey: string | null;
  open: boolean;
  tabs: CanvasTab[];
  active: string | null;
  working: Working | null;
  /** The saved arrangement; read it through `canvasLayout.effective` (it may name tabs not open yet). */
  layout: PaneLayout;
  /** The last view opened: where `beside`/`below` land when nothing is focused. */
  lastOpened: string | null;
}

/** One card in the conversation, as the feed describes it. */
export interface FeedCard {
  /** Stable per entry: `${id}@${version}`. */
  key: string;
  ref: ArtifactRef;
  /** Arrived after the view mounted (not history). */
  live: boolean;
  /** The turn it belongs to started with an operator message. */
  operatorTurn: boolean;
  /** The turn (queued message id) the card belongs to. */
  turn: string | null;
}

/** A `view_ref` card in the conversation: a reference to an app screen. */
export interface FeedView {
  /** Stable per entry. */
  key: string;
  ref: AppViewRef;
  /** CAD-2635: where the card asks for the view; absent or unknown: today's behaviour. */
  placement?: Placement;
  /** The card is for the app page already on screen: it moves that page (`showHere`), never a canvas tab. */
  here?: true;
  live: boolean;
  operatorTurn: boolean;
  turn: string | null;
}

/** A `detail` record card in the conversation (CAD-2543). */
export interface FeedRecord {
  /** Stable per entry. */
  key: string;
  ref: RecordViewRef;
  live: boolean;
  operatorTurn: boolean;
  turn: string | null;
}

export interface FeedInput {
  conversation: string | null;
  surface: Surface;
  composerKey: string | null;
  cards: FeedCard[];
  /** `view_ref` cards (CAD-2475); absent: none. */
  views?: FeedView[];
  /** `detail` record cards (CAD-2543); absent: none. */
  records?: FeedRecord[];
  /** Moves the live app page to a route (CAD-2512). */
  showHere?: (route: AppViewRef["route"]) => void;
  working: Working | null;
  /** The turn in flight, if any — a close is remembered against it. */
  turn: string | null;
  /** Default true. False: no card ever opens the drawer by itself. */
  autoOpen?: boolean;
}

const listeners = new Set<() => void>();
let state: CanvasState = emptyState();
/** Per conversation bookkeeping; reset when the conversation changes. */
let seen = new Set<string>();
let closedTurns = new Set<string>();
let autoOpened = new Set<string>();
let currentTurn: string | null = null;

function emptyState(
  conversation: string | null = null,
  composerKey: string | null = null,
  surface: Surface = "home",
): CanvasState {
  return { conversation, surface, composerKey, open: false, tabs: [], active: null, working: null, layout: readLayout(conversation), lastOpened: null };
}

/** An artifact took the active tab: "user" for a card or tab click, "auto"
 *  for the feed's one auto-open per turn. The session tab (browserStore.ts) is
 *  not modelled here; it listens instead. */
export type FocusSource = "user" | "auto";
const artifactFocusListeners = new Set<(source: FocusSource) => void>();
export function onArtifactFocus(listener: (source: FocusSource) => void): void {
  artifactFocusListeners.add(listener);
}
function artifactFocused(source: FocusSource): void {
  for (const l of [...artifactFocusListeners]) l(source);
}
/** The last artifact tab closed. A listener answers true to keep the drawer
 *  open on something else it shows (the Browser tab). */
const emptiedListeners = new Set<() => boolean>();
export function onArtifactsEmptied(listener: () => boolean): void {
  emptiedListeners.add(listener);
}

function set(next: CanvasState, save = true): void {
  if (next === state) return;
  if (save && next.layout !== state.layout) writeLayout(next.conversation, next.layout);
  state = next;
  for (const l of [...listeners]) l();
}

export function canvasSnapshot(): CanvasState {
  return state;
}

export function subscribeCanvas(listener: () => void): () => void {
  listeners.add(listener);
  return () => listeners.delete(listener);
}

export function useCanvas(): CanvasState {
  return useSyncExternalStore(subscribeCanvas, canvasSnapshot, canvasSnapshot);
}

/** Test and conversation-switch reset. */
export function resetCanvas(
  conversation: string | null = null,
  composerKey: string | null = null,
  surface: Surface = "home",
): void {
  seen = new Set();
  closedTurns = new Set();
  autoOpened = new Set();
  currentTurn = null;
  set(emptyState(conversation, composerKey, surface), false);
}

function sameWorking(a: Working | null, b: Working | null): boolean {
  return a === b || (a !== null && b !== null && JSON.stringify(a) === JSON.stringify(b));
}

function newTab(ref: ArtifactRef, turn: string | null): CanvasTab {
  return {
    id: ref.id,
    kind: ref.kind,
    title: ref.title,
    bytes: ref.bytes,
    ...(ref.aspect ? { aspect: ref.aspect } : {}),
    latest: ref.version,
    selected: ref.version,
    follow: true,
    versions: { [ref.version]: { bytes: ref.bytes, digest: ref.digest } },
    turn,
  };
}

function withVersion(t: CanvasTab, ref: ArtifactRef, turn: string | null): CanvasTab {
  const newer = ref.version > t.latest;
  return {
    ...t,
    versions: { ...t.versions, [ref.version]: { bytes: ref.bytes, digest: ref.digest } },
    ...(ref.aspect && !t.aspect ? { aspect: ref.aspect } : {}),
    ...(newer
      ? { title: ref.title, bytes: ref.bytes, latest: ref.version, selected: t.follow ? ref.version : t.selected, turn }
      : {}),
  };
}

const idsOf = (tabs: CanvasTab[]): string[] => tabs.map((t) => t.id);

interface Stage {
  layout: PaneLayout;
  active: string | null;
}

/**
 * Bring tab `id` (already in `tabs`) on stage. `beside`/`below` anchor to the
 * focused pane, else the last opened view; with four panes showing the view
 * stays a background tab. With panes showing, a view with no placement takes
 * the focused pane's place (the one that was there returns to its tab) and
 * an explicit `tab` stays in the strip. With no panes it is simply the shown tab.
 */
function present(tabs: CanvasTab[], active: string | null, lastOpened: string | null, layout: PaneLayout, id: string, placement?: Placement): Stage {
  const ids = idsOf(tabs);
  const live = livePanes(layout, ids);
  const focused = active !== null && live.includes(active) ? active : (live[0] ?? null);
  if (placement === "beside" || placement === "below") {
    if (live.includes(id)) return { layout, active: id };
    if (live.length >= MAX_PANES) return { layout, active };
    const others = ids.filter((x) => x !== id);
    const anchor = [focused, active, lastOpened].find((x) => x !== null && others.includes(x)) ?? others[others.length - 1] ?? null;
    const placed = addPane(layout, ids, id, anchor, placement === "beside" ? "split" : "stack");
    return { layout: placed.layout, active: id };
  }
  if (live.length < 2) return { layout, active: id };
  if (placement === "tab") return { layout, active };
  if (live.includes(id)) return { layout, active: id };
  const swapped = replacePane(layout, ids, focused, id);
  return { layout: swapped.layout, active: swapped.placed ? id : active };
}

/** With panes showing, the active view is one of them. */
function onStage(tabs: CanvasTab[], active: string | null, layout: PaneLayout): string | null {
  const live = livePanes(layout, idsOf(tabs));
  return live.length >= 2 && (active === null || !live.includes(active)) ? live[0] : active;
}

/**
 * Fold the conversation's cards and working state in. Idempotent: the
 * feed calls it on every render of the thread, so only a card never seen
 * before can add a tab, move a version or open the drawer.
 */
export function feedCanvas(input: FeedInput): void {
  if (input.conversation !== state.conversation) resetCanvas(input.conversation, input.composerKey, input.surface);
  currentTurn = input.turn;
  let tabs = state.tabs;
  let open = state.open;
  let active = state.active;
  let layout = state.layout;
  let lastOpened = state.lastOpened;
  let focused = false;
  const show = (id: string, placement?: Placement) => {
    ({ layout, active } = present(tabs, active, lastOpened, layout, id, placement));
    lastOpened = id;
    open = true;
    focused = true;
  };
  // CAD-2548: a live card of the turn still in flight waits for that turn to
  // end (the thread paints its cards with the answer); it is not "seen" yet.
  const held = (x: { live: boolean; turn: string | null }) => x.live && x.turn !== null && x.turn === (input.working?.turn ?? null);
  for (const card of input.cards) {
    if (seen.has(card.key)) continue;
    if (!held(card)) seen.add(card.key);
    const { ref } = card;
    // A card only ever updates a tab that is already open. A tab is added by
    // opening a card, or by the one auto-open per operator turn (CAD-651).
    if (tabs.some((t) => t.id === ref.id)) {
      tabs = tabs.map((t) => (t.id === ref.id && !t.app ? withVersion(t, ref, card.turn) : t));
    }
    if (input.autoOpen !== false && card.live && !held(card) && card.operatorTurn && card.turn && !autoOpened.has(card.turn) && !closedTurns.has(card.turn)) {
      autoOpened.add(card.turn);
      if (!tabs.some((t) => t.id === ref.id)) tabs = [...tabs, newTab(ref, card.turn)];
      show(ref.id);
    }
  }
  for (const view of input.views ?? []) {
    if (seen.has(view.key) || held(view)) continue;
    seen.add(view.key);
    // An explicit placement is Nova opening that view on purpose, so it is not the turn's one auto-open.
    if (input.autoOpen === false || !view.live || !view.operatorTurn || !view.turn || closedTurns.has(view.turn) || (autoOpened.has(view.turn) && view.placement === undefined)) continue;
    autoOpened.add(view.turn);
    if (view.here) {
      input.showHere?.(view.ref.route);
      continue;
    }
    const id = appViewId(view.ref);
    if (!tabs.some((t) => t.id === id)) tabs = [...tabs, appTab(id, view.ref)];
    show(id, view.placement);
  }
  for (const record of input.records ?? []) {
    if (seen.has(record.key) || held(record)) continue;
    seen.add(record.key);
    if (input.autoOpen === false || !record.live || !record.operatorTurn || !record.turn || autoOpened.has(record.turn) || closedTurns.has(record.turn)) continue;
    autoOpened.add(record.turn);
    if (!tabs.some((t) => t.id === record.ref.id)) tabs = [...tabs, recordTab(record.ref)];
    show(record.ref.id);
  }
  if (tabs.length === 0) {
    open = false;
    active = null;
  } else if (active === null || !tabs.some((t) => t.id === active)) {
    active = tabs[0].id;
  }
  active = onStage(tabs, active, layout);
  if (focused) artifactFocused("auto");
  const composerKey = input.composerKey;
  const working = sameWorking(input.working, state.working) ? state.working : input.working;
  if (
    tabs === state.tabs && open === state.open && active === state.active && layout === state.layout && lastOpened === state.lastOpened &&
    composerKey === state.composerKey && working === state.working && input.surface === state.surface
  ) return;
  set({ ...state, tabs, open, active, layout, lastOpened, composerKey, working, surface: input.surface });
}

/** Open (or focus) an artifact from its card. */
export function openArtifact(ref: ArtifactRef): void {
  const existing = state.tabs.find((t) => t.id === ref.id);
  const tabs = existing
    ? state.tabs.map((t) =>
        t.id !== ref.id || t.app
          ? t
          : {
              ...withVersion(t, ref, t.turn),
              selected: ref.version,
              follow: ref.version >= Math.max(t.latest, ref.version),
            },
      )
    : [...state.tabs, newTab(ref, null)];
  set({ ...state, tabs, ...present(tabs, state.active, state.lastOpened, state.layout, ref.id), lastOpened: ref.id, open: true });
  artifactFocused("user");
}

function appTab(id: string, ref: AppViewRef): CanvasTab {
  return { id, kind: "app", title: ref.title, app: ref, latest: 1, selected: 1, follow: false, versions: {}, turn: null };
}

function recordTab(ref: RecordViewRef): CanvasTab {
  return { id: ref.id, kind: "record", title: ref.title, record: ref, latest: 1, selected: 1, follow: false, versions: {}, turn: null };
}

/** Open (or focus) a `detail` record card's reader from the card. */
export function openRecordView(ref: RecordViewRef): void {
  const tabs = state.tabs.some((t) => t.id === ref.id) ? state.tabs : [...state.tabs, recordTab(ref)];
  set({ ...state, tabs, ...present(tabs, state.active, state.lastOpened, state.layout, ref.id), lastOpened: ref.id, open: true });
  artifactFocused("user");
}

/** Open (or focus) an installed app's screen from a resource link. A
 *  malformed reference opens nothing and says so. */
export function openAppView(input: unknown, placement?: Placement): boolean {
  const ref = appViewRef(input);
  if (!ref) return false;
  const id = appViewId(ref);
  // An open tab that moved inside its frame goes back to the card's own route.
  const tabs = state.tabs.some((t) => t.id === id)
    ? state.tabs.map((t) => {
        if (t.id !== id || !t.app || (sameRoute(t.app.route, ref.route) && t.app.viewId === ref.viewId)) return t;
        // The declared view (CAD-2710) travels with the route: two views can share a section and record.
        const { viewId: _viewId, viewLabel: _viewLabel, ...rest } = t.app;
        return { ...t, title: ref.title, app: { ...rest, route: ref.route, ...(ref.viewId === undefined ? {} : { viewId: ref.viewId }), ...(ref.viewLabel === undefined ? {} : { viewLabel: ref.viewLabel }) } };
      })
    : [...state.tabs, appTab(id, ref)];
  set({ ...state, tabs, ...present(tabs, state.active, state.lastOpened, state.layout, id, placement), lastOpened: id, open: true });
  artifactFocused("user");
  return true;
}

/** The frame navigated inside its tab. The route stays on the tab: the
 *  board URL is the full-page app's, and is never written from here. */
export function patchAppViewRoute(id: string, patch: RoutePatch): void {
  set({
    ...state,
    tabs: state.tabs.map((t) => {
      if (!t.app || t.id !== id) return t;
      const route = applyRoutePatch(t.app.route, patch);
      // Navigating to another section or record leaves the declared view (CAD-2710); a params-only change stays in it.
      const same = route.section === t.app.route.section && route.record_id === t.app.route.record_id;
      const { viewId: _viewId, viewLabel: _viewLabel, ...rest } = t.app;
      return { ...t, title: appViewTitle(t.id, t.app.title, route), app: same ? { ...t.app, route } : { ...rest, route } };
    }),
  });
}

export function focusTab(id: string): void {
  if (!state.tabs.some((t) => t.id === id)) return;
  const shown = present(state.tabs, state.active, state.lastOpened, state.layout, id);
  if (shown.active !== state.active || shown.layout !== state.layout || !state.open) set({ ...state, ...shown, open: true });
  artifactFocused("user");
}

export function closeTab(id: string): void {
  const index = state.tabs.findIndex((t) => t.id === id);
  if (index < 0) return;
  const tabs = state.tabs.filter((t) => t.id !== id);
  // The view is closed, so its place in the arrangement goes too; fewer than two saved is no arrangement.
  const saved = state.layout.panes.filter((p) => p !== id);
  const layout = saved.length === state.layout.panes.length ? state.layout : saved.length < 2 ? { ...state.layout, mode: "tabs" as const, panes: [] } : { ...state.layout, panes: saved };
  if (tabs.length === 0) {
    const kept = [...emptiedListeners].some((l) => l());
    if (!kept) closeDrawerFor(currentTurn);
    set({ ...state, tabs, layout, active: null, open: false });
    return;
  }
  const before = livePanes(state.layout, idsOf(state.tabs));
  const rest = before.filter((p) => p !== id);
  // A pane that closes hands focus to the pane beside it, and the last one left becomes the shown view.
  const next = before.includes(id) && rest.length > 0 && (rest.length === 1 || state.active === id) ? rest[Math.min(before.indexOf(id), rest.length - 1)] : null;
  const active = next ?? (state.active === id ? tabs[Math.min(index, tabs.length - 1)].id : state.active);
  set({ ...state, tabs, layout, active: onStage(tabs, active, layout) });
}

// ---- the arrangement (CAD-2635) ----

/** The layout switcher. Tabs returns every pane to the strip; the others arrange the open views. */
export function setLayoutMode(mode: LayoutMode): void {
  const layout = chooseMode(state.layout, idsOf(state.tabs), state.active, mode);
  if (layout === state.layout) return;
  set({ ...state, layout, active: onStage(state.tabs, state.active, layout) });
}

/** A pane's close (and "Move to tab"): the view goes back to a tab. */
export function unpane(id: string): void {
  const ids = idsOf(state.tabs);
  const before = livePanes(state.layout, ids);
  if (!before.includes(id)) return;
  const layout = removePane(state.layout, ids, id);
  const rest = before.filter((p) => p !== id);
  const active = rest.length === 1 || state.active === id ? rest[Math.min(before.indexOf(id), rest.length - 1)] : state.active;
  set({ ...state, layout, active });
}

/** A tab dropped on the stage: left, right or bottom of the focused view, or in its place. */
export function dropTab(id: string, zone: DropZone): boolean {
  const anchor = state.active !== null && state.active !== id ? state.active : null;
  const dropped = dropPane(state.layout, idsOf(state.tabs), id, anchor, zone);
  if (!dropped.placed) return false;
  set({ ...state, layout: dropped.layout, active: id, lastOpened: id });
  return true;
}

/** A divider moved. `commit` false while a drag is under way: the share is shown, not yet saved. */
export function setPaneShare(axis: "cols" | "rows", share: number, commit = true): void {
  const next = clampShare(share);
  if (next === state.layout[axis]) {
    if (commit) writeLayout(state.conversation, state.layout);
    return;
  }
  set({ ...state, layout: { ...state.layout, [axis]: next } }, commit);
}

function closeDrawerFor(turn: string | null): void {
  if (turn) closedTurns.add(turn);
}

/** The operator closed the drawer: it stays closed for this turn. */
export function closeDrawer(): void {
  if (!state.open) return;
  closeDrawerFor(currentTurn);
  set({ ...state, open: false });
}

/** Show the drawer again over the tabs it already holds (the Browser tab
 *  was hiding itself out of a drawer that it alone had opened). */
export function showDrawer(): void {
  if (state.tabs.length > 0 && !state.open) set({ ...state, open: true });
}

// A confirmed identity change drops this conversation's tabs: titles and
// versions belong to the person who was signed in.
onPrivateSessionReset(() => {
  clearSavedLayouts();
  resetCanvas();
});

export function selectVersion(id: string, version: number): void {
  set({
    ...state,
    tabs: state.tabs.map((t) => (t.id !== id || t.app ? t : { ...t, selected: version, follow: version >= t.latest })),
  });
}

// ---- width (remembered per browser; every storage access is guarded) ----

const WIDTH_KEY = "cadence.canvas.width";
export const MIN_WIDTH = 360;

/** `floor` lifts the minimum (a kind's default floor), and with it the cap
 *  when 60% of a narrow viewport would sit under that floor. */
export function clampWidth(px: number, viewport: number, floor = MIN_WIDTH): number {
  const max = Math.max(floor, Math.floor(viewport * 0.6));
  return Math.min(max, Math.max(floor, Math.round(px)));
}

/** The width when the operator has not dragged one: it scales with the
 *  viewport, wider for kinds that are read as a page. */
export function defaultWidth(kind: TabKind, viewport: number): number {
  const [share, lo, hi] = defaultRange(kind);
  return Math.min(hi, Math.max(lo, Math.round(viewport * share)));
}

function defaultRange(kind: TabKind): [number, number, number] {
  return kind === "app" || kind === "html" || kind === "csv" || kind === "pdf" ? [0.55, 560, 960] : [0.45, 480, 760];
}

/** The default width, held to its kind's floor even where 60% of the
 *  viewport is smaller (a 900px window keeps a document at 560px). */
export function defaultWidthFor(kind: TabKind, viewport: number): number {
  return clampWidth(defaultWidth(kind, viewport), viewport, defaultRange(kind)[1]);
}

/** The width the operator dragged to, or null (it wins over the default). */
export function readWidth(): number | null {
  try {
    const raw = globalThis.localStorage?.getItem(WIDTH_KEY);
    if (raw === null || raw === undefined) return null;
    const n = Number(raw);
    return Number.isFinite(n) && n >= MIN_WIDTH ? n : null;
  } catch {
    return null;
  }
}

export function writeWidth(px: number): void {
  try {
    globalThis.localStorage?.setItem(WIDTH_KEY, String(px));
  } catch {
    // Blocked storage: the width just is not remembered.
  }
}

/** push | cover | sheet — by viewport and the room the chat keeps. */
export type CanvasMode = "push" | "cover" | "sheet";
export const SHEET_BELOW = 640;
export const CHAT_MIN = 320;
const SIDEBAR = 208;
/** The board's folded sidebar (styles.css), which stays beside a Home canvas that is expanded. */
export const SIDEBAR_RAIL = 64;

export interface DrawerLayout {
  mode: CanvasMode;
  width: number;
  /** App canvases end before the right-docked assistant. Home stays flush right. */
  right?: number;
  /** Expanded beyond the workspace: the canvas covers the chat, which keeps a rail of its own. */
  full?: true;
}

/**
 * App canvases cover the workspace without resizing its frame or the right
 * assistant. Narrow app views use a sheet. Home retains its push/cover rule.
 */
export function drawerLayout(
  viewport: number,
  want: number,
  surface: Surface,
  workspace: { left: number; right: number } | null,
  expanded = false,
): DrawerLayout {
  if (viewport < SHEET_BELOW || (surface === "app" && viewport < 1024)) {
    return { mode: "sheet", width: viewport };
  }
  if (surface === "app" && workspace !== null) {
    const room = workspace.right - workspace.left;
    if (room < MIN_WIDTH) return { mode: "sheet", width: viewport };
    return { mode: "cover", width: expanded ? room : Math.min(want, room), right: Math.max(0, viewport - workspace.right) };
  }
  // Expanded (CAD-2657): the canvas takes the whole window beside the folded
  // sidebar; its own left rail holds the chat.
  if (expanded) return { mode: "cover", width: viewport - (viewport >= 1024 ? SIDEBAR_RAIL : 0), full: true };
  const main = viewport - (viewport >= 1024 ? SIDEBAR : 0);
  const need = surface === "home" ? CHAT_MIN : CHAT_MIN + 320 + 42;
  if (main - want >= need) return { mode: "push", width: want };
  // Without an app workspace, retain the page-based cap.
  const cap = main - CHAT_MIN - 28;
  if (cap < MIN_WIDTH) return { mode: "sheet", width: viewport };
  return { mode: "cover", width: Math.min(want, cap) };
}
