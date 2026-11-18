import { useEffect, useRef, useState, type KeyboardEvent } from "react";
import { useTranslate } from "../lib/localeText";
import "./controls.css";

export interface RowMenuItem {
  key: string;
  label: string;
  /** A second, quieter line. */
  hint?: string;
  danger?: boolean;
  onSelect: () => void;
}

/**
 * CAD-2438: a ⋯ menu for one row. Opens on the first item; ArrowUp/Down/Home/End
 * move, Escape and Tab close it and Escape (and a choice) returns focus to the
 * trigger. Escape is consumed here, so a drawer around it stays open.
 */
export default function RowMenu({ label, items }: { label: string; items: RowMenuItem[] }) {
  const t = useTranslate();
  const [open, setOpen] = useState(false);
  const wrap = useRef<HTMLDivElement | null>(null);
  const trigger = useRef<HTMLButtonElement | null>(null);
  const entries = () => Array.from(wrap.current?.querySelectorAll<HTMLElement>('[role="menuitem"]') ?? []);
  useEffect(() => {
    if (!open) return;
    entries()[0]?.focus();
    const away = (e: MouseEvent) => {
      if (wrap.current && !wrap.current.contains(e.target as Node)) setOpen(false);
    };
    document.addEventListener("mousedown", away);
    return () => document.removeEventListener("mousedown", away);
  }, [open]);
  const close = (refocus: boolean) => {
    setOpen(false);
    if (refocus) trigger.current?.focus();
  };
  const onKey = (e: KeyboardEvent) => {
    if (!open) return;
    if (e.key === "Escape") {
      e.preventDefault();
      e.stopPropagation();
      close(true);
      return;
    }
    if (e.key === "Tab") {
      setOpen(false);
      return;
    }
    const els = entries();
    const at = els.indexOf(document.activeElement as HTMLElement);
    const next =
      e.key === "ArrowDown" ? (at + 1) % els.length
      : e.key === "ArrowUp" ? (at - 1 + els.length) % els.length
      : e.key === "Home" ? 0
      : e.key === "End" ? els.length - 1
      : -1;
    if (next < 0) return;
    e.preventDefault();
    els[next]?.focus();
  };
  return (
    <div className="row-menu" ref={wrap} onKeyDown={onKey}>
      <button
        ref={trigger}
        type="button"
        className="btn btn-sm btn-ghost btn-icon"
        aria-label={label}
        aria-haspopup="menu"
        aria-expanded={open}
        onClick={() => setOpen(!open)}
        onKeyDown={(e) => {
          if (e.key === "ArrowDown" && !open) {
            e.preventDefault();
            setOpen(true);
          }
        }}
      >
        ⋯
      </button>
      {open && (
        <div className="row-menu-pop" role="menu" aria-label={label}>
          {items.map((item, i) => (
            <div key={item.key} role="none">
              {item.danger && i > 0 && <hr />}
              <button
                type="button"
                role="menuitem"
                className={item.danger ? "danger" : undefined}
                onClick={() => {
                  close(true);
                  item.onSelect();
                }}
              >
                {t(item.label)}
                {item.hint && <small>{item.hint}</small>}
              </button>
            </div>
          ))}
        </div>
      )}
    </div>
  );
}
