import { useSyncExternalStore } from "react";
import { followClientNav } from "./clientNav";
import { scopeRedirect } from "./router";
import { browserStoredProjectView } from "./urlState";

/**
 * The browser location as a store: `navigate` pushes (or replaces) a history
 * entry and every subscriber re-reads; back and forward arrive as
 * `popstate`. A pre-router URL reached through history is redirected in
 * place, like one typed in (see `applyLegacyRedirect`).
 */

const listeners = new Set<() => void>();

function notify(): void {
  for (const listener of listeners) listener();
}

/** Rewrite a legacy `?tab=` URL, and drop `?project=` from a screen that
 *  has no project scope, without a new history entry. */
export function applyLegacyRedirect(): void {
  const next = scopeRedirect(location.pathname, location.search, browserStoredProjectView());
  if (next) history.replaceState(history.state, "", next);
}

function subscribe(listener: () => void): () => void {
  if (listeners.size === 0) addEventListener("popstate", onPopState);
  listeners.add(listener);
  return () => {
    listeners.delete(listener);
    if (listeners.size === 0) removeEventListener("popstate", onPopState);
  };
}

function onPopState(): void {
  applyLegacyRedirect();
  notify();
}

const snapshot = () => location.pathname + location.search;

export function navigate(href: string, opts: { replace?: boolean } = {}): void {
  if (href === snapshot()) return;
  if (opts.replace) history.replaceState(null, "", href);
  else history.pushState(null, "", href);
  notify();
}

/** The current path plus query; re-renders on every navigation. */
export function useHref(): string {
  return useSyncExternalStore(subscribe, snapshot);
}

let clientNavInstalled = false;

/**
 * One listener for the whole app: same-origin anchors navigate through
 * `navigate` instead of loading a new document. Installed once from
 * `main.tsx`. Link's own click handler still runs first and may
 * `preventDefault` (section tabs replace history); this listener then
 * leaves that click alone.
 */
export function installClientNav(): void {
  if (clientNavInstalled || typeof document === "undefined") return;
  clientNavInstalled = true;
  document.addEventListener("click", (event) => {
    const el = event.target instanceof Element ? event.target.closest("a") : null;
    if (!el) return;
    followClientNav(
      {
        href: el.getAttribute("href"),
        download: el.hasAttribute("download"),
        target: el.getAttribute("target"),
        button: event.button,
        metaKey: event.metaKey,
        ctrlKey: event.ctrlKey,
        shiftKey: event.shiftKey,
        altKey: event.altKey,
        defaultPrevented: event.defaultPrevented,
        origin: location.origin,
      },
      () => event.preventDefault(),
      (href) => navigate(href),
    );
  });
}
