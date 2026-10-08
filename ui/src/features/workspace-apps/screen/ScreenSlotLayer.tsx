import { useEffect, useRef, useState } from "react";
import SafeLink from "../../../ui/SafeLink";
import { slotBox, type SlotView } from "./screenSlot";

/**
 * The host-drawn parts over a mounted frame (CAD-1123 HP3), rendered in the
 * parent page so the sandboxed frame can neither click nor restyle them: the
 * approval control the frame asked for, anchored to its requested rectangle,
 * and the link chip `open-link` raises. The label comes only from host data.
 * The press time comes from the browser's own `pointerdown` event; the guard
 * itself is the controller's (500 ms after the slot appears or moves).
 */
export default function ScreenSlotLayer({ view, link, onTap, onDismissLink }: {
  view: SlotView | null; link: string | null;
  onTap: (token: string, trusted: boolean, pressedAt: number) => boolean; onDismissLink: () => void;
}) {
  const layer = useRef<HTMLDivElement>(null);
  const pressed = useRef(0);
  const [size, setSize] = useState({ w: 0, h: 0 });
  useEffect(() => {
    const node = layer.current;
    if (!node) return;
    const measure = () => setSize({ w: node.clientWidth, h: node.clientHeight });
    measure();
    if (typeof ResizeObserver === "undefined") return;
    const observer = new ResizeObserver(measure);
    observer.observe(node);
    return () => observer.disconnect();
  }, []);
  const box = view ? slotBox(view.anchor, size.w, size.h) : null;
  return (
    <div ref={layer} className="screen-slot-layer" style={{ position: "absolute", inset: 0, pointerEvents: "none", zIndex: 5 }}>
      {view && box && (
        <div data-host-slot={view.token.slice(0, 8)} style={{ position: "absolute", left: box.x, top: box.y, width: box.w, height: box.h,
          pointerEvents: "auto", display: "flex", alignItems: "center", justifyContent: "center",
          padding: box.footer ? "8px 16px" : 0, background: box.footer ? "var(--surface, var(--ink-900, #111))" : "transparent",
          borderTop: box.footer ? "1px solid var(--line, rgba(128,128,128,.3))" : undefined }}>
          {view.ownerPortalUrl ? (
            <a href={view.ownerPortalUrl} target="_blank" rel="noopener noreferrer"
              className="btn btn-primary" aria-disabled={view.pending} aria-busy={view.pending}
              style={{ display: "inline-flex", alignItems: "center", justifyContent: "center",
                width: box.footer ? "min(100%, 320px)" : "100%", height: box.footer ? 40 : "100%",
                minWidth: 0, overflow: "hidden", textOverflow: "ellipsis", whiteSpace: "nowrap" }}
              onPointerDown={event => { pressed.current = event.timeStamp; }}
              onClick={event => {
                if (event.button !== 0 || event.metaKey || event.ctrlKey || event.shiftKey || event.altKey) {
                  event.preventDefault();
                  return;
                }
                // Keyboard activation has no pointerdown: it counts as pressed now.
                const at = event.detail === 0 ? performance.now() : pressed.current;
                pressed.current = 0;
                if (!onTap(view.token, event.isTrusted, at)) event.preventDefault();
              }}
              onAuxClick={event => event.preventDefault()}
              onContextMenu={event => event.preventDefault()}>
              {view.pending ? "Working…" : view.label}
            </a>
          ) : (
            <button type="button" className="btn btn-primary" disabled={view.pending} aria-busy={view.pending}
              style={{ width: box.footer ? "min(100%, 320px)" : "100%", height: box.footer ? 40 : "100%", minWidth: 0, overflow: "hidden", textOverflow: "ellipsis", whiteSpace: "nowrap" }}
              onPointerDown={event => { pressed.current = event.timeStamp; }}
              onClick={event => {
                // Keyboard activation has no pointerdown: it counts as pressed now.
                const at = event.detail === 0 ? performance.now() : pressed.current;
                pressed.current = 0;
                onTap(view.token, event.isTrusted, at);
              }}>
              {view.pending ? "Working…" : view.label}
            </button>
          )}
        </div>
      )}
      {link && (
        <div role="status" style={{ position: "absolute", left: 12, bottom: view && box?.footer ? 68 : 12, maxWidth: "calc(100% - 24px)", pointerEvents: "auto",
          display: "flex", gap: 8, alignItems: "center", padding: "6px 10px", borderRadius: 8,
          background: "var(--surface, var(--ink-900, #111))", border: "1px solid var(--line, rgba(128,128,128,.3))" }}>
          <SafeLink href={link} className="text-label underline break-all">Open link</SafeLink>
          <button type="button" className="btn btn-ghost btn-sm" aria-label="Dismiss link" onClick={onDismissLink}>×</button>
        </div>
      )}
    </div>
  );
}
