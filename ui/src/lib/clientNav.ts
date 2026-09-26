/**
 * In-app clicks (CAD-609). A plain left click on a same-origin link
 * stays in the SPA — `history.pushState`, not a document load — so the
 * wiki tree keeps the folders it has expanded. Open-in-new-tab, a
 * download, and `/api/` (file bytes live there) keep the browser's own
 * navigation. Real `href`s stay on the anchor.
 */

export interface ClientNavClick {
  href: string | null;
  download: boolean;
  target: string | null;
  button: number;
  metaKey: boolean;
  ctrlKey: boolean;
  shiftKey: boolean;
  altKey: boolean;
  defaultPrevented: boolean;
  /** The page origin, e.g. `http://127.0.0.1:3111`. */
  origin: string;
}

/** The href to hand to `navigate`, or null when the click should load for real. */
export function clientNavHref(click: ClientNavClick): string | null {
  if (click.defaultPrevented || click.download || click.button !== 0) return null;
  if (click.metaKey || click.ctrlKey || click.shiftKey || click.altKey) return null;
  const target = (click.target ?? "").trim().toLowerCase();
  if (target && target !== "_self") return null;
  const raw = click.href?.trim() ?? "";
  // Hash-only stays with the browser (in-page jump, no reload of the app).
  if (!raw || raw.startsWith("#")) return null;
  let url: URL;
  try {
    url = new URL(raw, click.origin);
  } catch {
    return null;
  }
  if (url.origin !== click.origin) return null;
  if (url.pathname === "/api" || url.pathname.startsWith("/api/")) return null;
  return `${url.pathname}${url.search}${url.hash}`;
}

/**
 * Prevent the document load and call `go` when the click stays in the
 * app. Returns whether it did — a false result means the browser proceeds.
 */
export function followClientNav(
  click: ClientNavClick,
  preventDefault: () => void,
  go: (href: string) => void,
): boolean {
  const href = clientNavHref(click);
  if (!href) return false;
  preventDefault();
  go(href);
  return true;
}
