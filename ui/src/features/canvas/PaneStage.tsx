/**
 * CAD-2635: the canvas stage. One grid holds every view on stage: the active
 * tab alone, or two to four panes (side by side, stacked, a grid) with
 * resizable dividers. Each view is one keyed child, placed by its grid cell and
 * never reordered, so moving a view between a tab and a pane does not move its
 * DOM node and the app frame inside it is not reloaded. Dividers answer to the
 * pointer and the arrow keys; a tab being dragged lights the drop zones, which
 * sit above the frames (an iframe would swallow the drop).
 */
import { useRef, useState, type CSSProperties, type DragEvent, type KeyboardEvent, type PointerEvent, type ReactNode } from "react";
import { useTranslate } from "../../lib/locale";
import { cellFor, dividersFor, effective, livePanes, MAX_PANES, nudgeShare, type DropZone, type PaneLayout } from "./canvasLayout";
import { dropTab, focusTab, setPaneShare, type CanvasTab } from "./canvasStore";

export const TAB_DRAG_TYPE = "application/x-cadence-canvas-tab";

function Divider({ axis, share, stage, label }: { axis: "cols" | "rows"; share: number; stage: React.RefObject<HTMLDivElement | null>; label: string }) {
  const vertical = axis === "cols";
  const move = (px: number, commit: boolean) => {
    const box = stage.current?.getBoundingClientRect();
    const size = vertical ? box?.width : box?.height;
    if (!box || !size) return;
    setPaneShare(axis, ((px - (vertical ? box.left : box.top)) / size), commit);
  };
  const dragging = useRef(false);
  const onPointerDown = (e: PointerEvent<HTMLDivElement>) => {
    dragging.current = true;
    e.currentTarget.setPointerCapture(e.pointerId);
  };
  const onPointerMove = (e: PointerEvent<HTMLDivElement>) => {
    if (dragging.current) move(vertical ? e.clientX : e.clientY, false);
  };
  const onPointerUp = (e: PointerEvent<HTMLDivElement>) => {
    if (!dragging.current) return;
    dragging.current = false;
    e.currentTarget.releasePointerCapture(e.pointerId);
    move(vertical ? e.clientX : e.clientY, true);
  };
  // The canvas grip's steps: 24px, 96px with Shift.
  const onKeyDown = (e: KeyboardEvent<HTMLDivElement>) => {
    const step = e.shiftKey ? 96 : 24;
    const back = vertical ? "ArrowLeft" : "ArrowUp";
    const forward = vertical ? "ArrowRight" : "ArrowDown";
    if (e.key !== back && e.key !== forward) return;
    const box = stage.current?.getBoundingClientRect();
    const size = (vertical ? box?.width : box?.height) ?? 0;
    setPaneShare(axis, nudgeShare(share, e.key === back ? -step : step, size));
    e.preventDefault();
  };
  return (
    <div
      className="cv-divider"
      data-axis={axis}
      role="separator"
      aria-orientation={vertical ? "vertical" : "horizontal"}
      aria-label={label}
      aria-valuemin={20}
      aria-valuemax={80}
      aria-valuenow={Math.round(share * 100)}
      tabIndex={0}
      onPointerDown={onPointerDown}
      onPointerMove={onPointerMove}
      onPointerUp={onPointerUp}
      onPointerCancel={onPointerUp}
      onKeyDown={onKeyDown}
    />
  );
}

function Zones({ dragged, full, onDone }: { dragged: string; full: boolean; onDone: () => void }) {
  const t = useTranslate();
  const [over, setOver] = useState<DropZone | null>(null);
  const zones: [DropZone, string, string][] = [
    ["left", t("Split left"), "left"],
    ["replace", t("Replace the focused view"), "replace"],
    ["right", t("Split right"), "right"],
    ["bottom", t("Place below"), "bottom"],
  ];
  return (
    <div className="cv-zones" data-zones>
      {zones.map(([zone, label, area]) => {
        const off = full && zone !== "replace";
        return (
          <div
            key={zone}
            className="cv-zone"
            data-zone={zone}
            data-over={over === zone || undefined}
            data-disabled={off || undefined}
            style={{ gridArea: area }}
            onDragOver={(e: DragEvent) => { if (!off) { e.preventDefault(); setOver(zone); } }}
            onDragLeave={() => setOver((z) => (z === zone ? null : z))}
            onDrop={(e: DragEvent) => { e.preventDefault(); setOver(null); if (!off) dropTab(dragged, zone); onDone(); }}
          >
            <b>{label}</b>
          </div>
        );
      })}
    </div>
  );
}

export default function PaneStage({ tabs, active, layout, multi, droppable, hidden, dragged, onDragDone, renderBody }: {
  tabs: CanvasTab[];
  active: string | null;
  layout: PaneLayout;
  /** The canvas is wide enough for panes; narrower shows the active tab alone. */
  multi: boolean;
  /** A dropped tab can make panes (now or once the canvas has widened). */
  droppable: boolean;
  /** The Browser tab is in front; the stage stays mounted behind it. */
  hidden: boolean;
  /** The id of a tab being dragged, if any. */
  dragged: string | null;
  /** A drop (or a drag that ended) clears `dragged`. */
  onDragDone: () => void;
  renderBody: (tab: CanvasTab, paned: boolean) => ReactNode;
}) {
  const t = useTranslate();
  const stage = useRef<HTMLDivElement | null>(null);
  const ids = tabs.map((x) => x.id);
  const shown = multi ? effective(layout, ids) : { mode: "tabs" as const, panes: [] as string[] };
  const paned = shown.panes.length >= 2;
  const onStage = paned ? shown.panes : active !== null && ids.includes(active) ? [active] : [];
  const lines = dividersFor(shown.mode, shown.panes.length);
  const c = Math.round(layout.cols * 100);
  const r = Math.round(layout.rows * 100);
  const style: CSSProperties = {
    gridTemplateColumns: lines.vertical ? `minmax(0, ${c}fr) 6px minmax(0, ${100 - c}fr)` : "minmax(0, 1fr) 0 0",
    gridTemplateRows: lines.horizontal ? `minmax(0, ${r}fr) 6px minmax(0, ${100 - r}fr)` : "minmax(0, 1fr) 0 0",
  };
  const live = livePanes(layout, ids);
  return (
    <div ref={stage} className="cv-panes" data-layout={shown.mode} data-panes={paned ? shown.panes.length : undefined} hidden={hidden} style={style}>
      {tabs.filter((x) => onStage.includes(x.id)).map((x) => {
        const cell = cellFor(shown.mode, shown.panes.length, shown.panes.indexOf(x.id));
        return (
          <div
            key={x.id}
            className="cv-pane"
            data-pane={x.id}
            data-focused={(paned && x.id === active) || undefined}
            hidden={hidden}
            style={{ gridColumn: `${cell.col[0]} / ${cell.col[1]}`, gridRow: `${cell.row[0]} / ${cell.row[1]}` }}
            // An app frame takes focus and pointer events itself: a focus or press inside moves the focus to this pane.
            onFocusCapture={() => { if (paned && x.id !== active) focusTab(x.id); }}
            onPointerDownCapture={() => { if (paned && x.id !== active) focusTab(x.id); }}
          >
            {renderBody(x, paned)}
          </div>
        );
      })}
      {paned && lines.vertical ? <div className="cv-divider-slot" style={{ gridColumn: 2, gridRow: `${lines.vertical[0]} / ${lines.vertical[1]}` }}><Divider axis="cols" share={layout.cols} stage={stage} label={t("Resize panes side by side")} /></div> : null}
      {paned && lines.horizontal ? <div className="cv-divider-slot" style={{ gridRow: 2, gridColumn: "1 / 4" }}><Divider axis="rows" share={layout.rows} stage={stage} label={t("Resize panes above and below")} /></div> : null}
      {droppable && dragged !== null && dragged !== active ? <Zones dragged={dragged} full={live.length >= MAX_PANES && !live.includes(dragged)} onDone={onDragDone} /> : null}
    </div>
  );
}
