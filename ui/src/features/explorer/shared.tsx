import { useEffect, useRef } from "react";
/**
 * The Explorer's shared bits (CAD-1129): the app glyph, trust chip and
 * install-state chip every screen reuses — so a card, the detail hero
 * and the home tile read the same way.
 */

/** A monogram tile — the app's first letter on a tinted tile, or its
 * `listing.icon` name as the label's shape. */
export function AppGlyph({ name, icon, size }: { name: string; icon?: string; size?: "sm" | "lg" }) {
  const letter = (name.trim()[0] ?? "A").toUpperCase();
  const cls = size === "lg" ? "w-16 h-16 rounded-2xl text-2xl" : size === "sm" ? "w-9 h-9 rounded-lg text-cardtitle" : "w-11 h-11 rounded-xl text-xl";
  return (
    <span
      aria-hidden
      title={icon ? `${name} · ${icon}` : name}
      className={`shrink-0 grid place-items-center bg-accent/15 text-accent font-semibold ${cls}`}
    >
      {letter}
    </span>
  );
}

/** Cadence-shipped vs an unreviewed Git app — the trust chip. */
export function TrustChip({ trust }: { trust?: "cadence" | "unreviewed" }) {
  if (trust === "cadence") return <span className="chip ok">Cadence</span>;
  return <span className="chip warn">Not reviewed by Cadence</span>;
}

/** Installed / off / requested / removed — the state a card wears. */
export function InstallStateChip({ state }: { state?: string }) {
  switch (state) {
    case "installed":
      return <span className="chip ok">Installed</span>;
    case "off":
      return <span className="chip">Access off</span>;
    case "requested":
      return <span className="chip">Requested</span>;
    case "removed":
      return <span className="chip">Removed</span>;
    default:
      return null;
  }
}

const FOCUSABLE =
  'a[href], button:not([disabled]), input:not([disabled]), select:not([disabled]), textarea:not([disabled]), [tabindex]:not([tabindex="-1"])';

/** Call `onEscape` when Escape is pressed while `active`. */
export function useEscape(active: boolean, onEscape: () => void): void {
  const latest = useRef(onEscape);
  latest.current = onEscape;
  useEffect(() => {
    if (!active) return;
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") latest.current();
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [active]);
}

/**
 * A modal dialog's keyboard contract (CAD-1209): focus moves into the
 * dialog when it opens, Tab and Shift+Tab wrap inside it, Escape closes it
 * and focus returns to whatever opened it. Attach the returned ref to the
 * dialog element (give it `tabIndex={-1}`).
 */
export function useModal<T extends HTMLElement>(open: boolean, onClose: () => void) {
  const ref = useRef<T>(null);
  const latest = useRef(onClose);
  latest.current = onClose;
  useEffect(() => {
    if (!open) return;
    const node = ref.current;
    if (!node) return;
    const opener = document.activeElement instanceof HTMLElement ? document.activeElement : null;
    const items = () => Array.from(node.querySelectorAll<HTMLElement>(FOCUSABLE));
    (items()[0] ?? node).focus();
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") {
        e.stopPropagation();
        latest.current();
        return;
      }
      if (e.key !== "Tab") return;
      const list = items();
      if (list.length === 0) { e.preventDefault(); node.focus(); return; }
      const first = list[0];
      const last = list[list.length - 1];
      const active = document.activeElement;
      if (e.shiftKey && (active === first || !node.contains(active))) { e.preventDefault(); last.focus(); }
      else if (!e.shiftKey && (active === last || !node.contains(active))) { e.preventDefault(); first.focus(); }
    };
    document.addEventListener("keydown", onKey, true);
    return () => {
      document.removeEventListener("keydown", onKey, true);
      if (opener && document.contains(opener)) opener.focus();
    };
  }, [open]);
  return ref;
}
