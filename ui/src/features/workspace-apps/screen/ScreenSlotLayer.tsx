import { useEffect, useRef, useState } from "react";

function RecordConfirmation({ view, card, onTap, onCancel }: {
  view: SlotView; card: RecordCard;
  onTap: (token: string, trusted: boolean, pressedAt: number, input?: string) => void;
  onCancel: (token: string) => void;
}) {
  const [input, setInput] = useState(card.input?.value ?? "");
  const pressed = useRef(0);
  const titles: Record<string, string> = {
    "records.csv.import": "Import this CSV plan?", "records.csv.confirm": "Confirm this CSV plan?", "content.approve": "Approve this content revision?",
    "smtp.manage": "Open email sending settings?", "smtp.bind": "Bind this SMTP connection?", "smtp.rebind": "Replace this SMTP binding?", "smtp.revoke": "Revoke this SMTP binding?",
    "smtp.test-send": "Send one test email?", "sends.approve": "Approve this frozen send?", "sends.resolve": "Resolve this uncertain delivery?",
    "origin.manage": "Change the daemon-global unsubscribe origin?", "chat.campaign.open": "Open this campaign's assistant?", "chat.campaign.brief": "Send this campaign brief?",
    "whatsapp.reply.send": "Send this WhatsApp reply?",
  };
  const field = card.input;
  const invalid = field?.choices ? !field.choices.some(c => c.value === input) : field?.field === "to_email" ? !/^[^\s@]+@[^\s@]+\.[^\s@]+$/.test(input) : false;
  return <WorkspaceDialog title={titles[card.operation] ?? "Confirm app action?"} className="wa-confirm" onClose={() => { if (!view.pending) onCancel(view.token); }}>
    <div className="wa-stack" data-host-confirm={view.token.slice(0, 8)}>
      <p>This is a Cadence host confirmation. Facts below came from the current server receipt, not the app frame.</p>
      <dl>{card.facts.map((fact, i) => <div key={i}><dt>{fact.label}</dt><dd style={{ whiteSpace: "pre-wrap", overflowWrap: "anywhere" }}>{fact.value}</dd></div>)}</dl>
      {field && <label className="wa-stack">{field.label}
        {field.choices ? <select value={input} onChange={e => setInput(e.target.value)} disabled={view.pending}><option value="">Select…</option>{field.choices.map(c => <option key={c.value} value={c.value}>{c.label}</option>)}</select>
          : field.field === "brief" ? <textarea value={input} readOnly rows={6} />
          : <input type={field.field === "to_email" ? "email" : "url"} value={input} maxLength={field.field === "to_email" ? 320 : 2048} onChange={e => setInput(e.target.value)} disabled={view.pending} />}
      </label>}
      <p>No automatic retry. If the result is uncertain, inspect fresh status before requesting another effect.</p>
      <div style={{ display: "flex", gap: 8, justifyContent: "flex-end" }}>
        <Button disabled={view.pending} onClick={() => onCancel(view.token)}>Cancel</Button>
        <button type="button" className="btn btn-primary" disabled={view.pending || invalid} aria-busy={view.pending}
          onPointerDown={e => { pressed.current = e.timeStamp; }} onClick={e => {
            const at = e.detail === 0 ? performance.now() : pressed.current; pressed.current = 0;
            onTap(view.token, e.isTrusted, at, input);
          }}>{view.pending ? "Working…" : view.label}</button>
      </div>
    </div>
  </WorkspaceDialog>;
}
import Button from "../../../ui/Button";
import { WorkspaceDialog } from "../WorkspaceDialog";
import { linkTarget } from "./screenActions";
import { slotBox, type RecordCard, type SlotView } from "./screenSlot";
import { useTranslate } from "../../../lib/locale";

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
  onTap: (token: string, trusted: boolean, pressedAt: number, input?: string) => void; onCancel: (token: string) => void; onDismissLink: () => void;
}) {
  const t = useTranslate();
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
  const box = view && !view.card && !view.recordCard ? slotBox(view.anchor, size.w, size.h) : null;
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
        <WorkspaceDialog title={t(view.card.title ?? "Post this now?")} className="wa-confirm" onClose={() => { if (!view.pending) onCancel(view.token); }}>
          <div className="wa-stack" data-host-confirm={view.token.slice(0, 8)}>
            {view.card.image && <img src={view.card.image} alt={t("The image that will post")} style={{ maxWidth: "100%", maxHeight: 240, objectFit: "contain", borderRadius: 8 }} />}
            <p className="wa-caption" style={{ whiteSpace: "pre-wrap", overflowWrap: "anywhere" }}>{view.card.caption}</p>
            <p>{t("To")} <strong>{view.card.target}</strong></p>
            {view.card.detail && <p>{view.card.detail}</p>}
            <div style={{ display: "flex", gap: 8, justifyContent: "flex-end", flexWrap: "wrap" }}>
              <Button disabled={view.pending} onClick={() => onCancel(view.token)}>{t("Cancel")}</Button>
              <button type="button" className="btn btn-primary" disabled={view.pending} aria-busy={view.pending}
                onPointerDown={event => { pressed.current = event.timeStamp; }}
                onClick={event => {
                  const at = event.detail === 0 ? performance.now() : pressed.current;
                  pressed.current = 0;
                  onTap(view.token, event.isTrusted, at);
                }}>
                {view.pending ? t("Posting…") : view.label}
              </button>
            </div>
          </div>
        </WorkspaceDialog>
      )}
      {view?.recordCard && <RecordConfirmation key={view.token} view={view} card={view.recordCard} onTap={onTap} onCancel={onCancel} />}
      {link && (
        <WorkspaceDialog title={t("Open this link?")} className="wa-confirm" onClose={onDismissLink}>
          <div className="wa-stack">
            <p>{t("This app wants to open a page on another site. It will open in a new tab.")}</p>
            <p><code className="num break-all" data-link-target>{linkTarget(link)}</code></p>
            <div style={{ display: "flex", gap: 8, justifyContent: "flex-end" }}>
              <Button onClick={onDismissLink}>{t("Cancel")}</Button>
              <Button variant="primary" onClick={() => { window.open(link, "_blank", "noopener,noreferrer"); onDismissLink(); }}>{t("Open")}</Button>
            </div>
          </div>
        </WorkspaceDialog>
      )}
    </div>
  );
}
