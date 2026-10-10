import { useEffect, useRef, useState } from "react";
import Button from "../../../ui/Button";
import { WorkspaceDialog } from "../WorkspaceDialog";
import { linkTarget } from "./screenActions";
import { slotBox, type SlotView } from "./screenSlot";

/**
 * The host-drawn parts over a mounted frame (CAD-1123 HP3), rendered in the
 * parent page so the sandboxed frame can neither click nor restyle them: the
 * approval button the frame asked for, anchored to its requested rectangle,
 * and the confirm `open-link` raises for an external link. The label comes only from host data.
 * The press time comes from the browser's own `pointerdown` event; the guard
 * itself is the controller's (500 ms after the slot appears or moves).
 */
export default function ScreenSlotLayer({ view, link, onTap, onCancel, onDismissLink }: {
  view: SlotView | null; link: string | null;
  onTap: (token: string, trusted: boolean, pressedAt: number) => void; onCancel: (token: string) => void; onDismissLink: () => void;
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
  const box = view && !view.card ? slotBox(view.anchor, size.w, size.h) : null;
  return (
    <div ref={layer} className="screen-slot-layer" style={{ position: "absolute", inset: 0, pointerEvents: "none", zIndex: 5 }}>
      {view && box && (
        <div data-host-slot={view.token.slice(0, 8)} style={{ position: "absolute", left: box.x, top: box.y, width: box.w, height: box.h,
          pointerEvents: "auto", display: "flex", alignItems: "center", justifyContent: "center",
          padding: box.footer ? "8px 16px" : 0, background: box.footer ? "var(--surface, var(--ink-900, #111))" : "transparent",
          borderTop: box.footer ? "1px solid var(--line, rgba(128,128,128,.3))" : undefined }}>
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
        </div>
      )}
      {view?.card && (
        <WorkspaceDialog title="Post this now?" className="wa-confirm" onClose={() => { if (!view.pending) onCancel(view.token); }}>
          <div className="wa-stack" data-host-confirm={view.token.slice(0, 8)}>
            {view.card.image && <img src={view.card.image} alt="The image that will post" style={{ maxWidth: "100%", maxHeight: 240, objectFit: "contain", borderRadius: 8 }} />}
            <p className="wa-caption" style={{ whiteSpace: "pre-wrap", overflowWrap: "anywhere" }}>{view.card.caption}</p>
            <p>To <strong>{view.card.target}</strong></p>
            <div style={{ display: "flex", gap: 8, justifyContent: "flex-end", flexWrap: "wrap" }}>
              <Button disabled={view.pending} onClick={() => onCancel(view.token)}>Cancel</Button>
              <button type="button" className="btn btn-primary" disabled={view.pending} aria-busy={view.pending}
                onPointerDown={event => { pressed.current = event.timeStamp; }}
                onClick={event => {
                  const at = event.detail === 0 ? performance.now() : pressed.current;
                  pressed.current = 0;
                  onTap(view.token, event.isTrusted, at);
                }}>
                {view.pending ? "Posting…" : view.label}
              </button>
            </div>
          </div>
        </WorkspaceDialog>
      )}
      {link && (
        <WorkspaceDialog title="Open this link?" className="wa-confirm" onClose={onDismissLink}>
          <div className="wa-stack">
            <p>This app wants to open a page on another site. It will open in a new tab.</p>
            <p><code className="num break-all" data-link-target>{linkTarget(link)}</code></p>
            <div style={{ display: "flex", gap: 8, justifyContent: "flex-end" }}>
              <Button onClick={onDismissLink}>Cancel</Button>
              <Button variant="primary" onClick={() => { window.open(link, "_blank", "noopener,noreferrer"); onDismissLink(); }}>Open</Button>
            </div>
          </div>
        </WorkspaceDialog>
      )}
    </div>
  );
}
