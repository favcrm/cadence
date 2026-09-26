import { useSyncExternalStore } from "react";
import { legacyRedirect } from "./router";
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

/** Rewrite a legacy `?tab=` URL to its route without a new history entry. */
export function applyLegacyRedirect(): void {
  // A static build cannot rewrite server paths, so a published preview
  // deep-links through the hash (`index.html#/apps/social-content/home`):
  // fold it into the real path and let the router see it.
  if (location.hash.startsWith("#/")) {
    history.replaceState(history.state, "", location.hash.slice(1) + location.search);
    return;
  }
  const next = legacyRedirect(location.pathname, location.search, browserStoredProjectView());
  if (next) history.replaceState(history.state, "", next);
}

function subscribe(listener: () => void): () => void {
  if (listeners.size === 0) {
    addEventListener("popstate", onPopState);
    addEventListener("hashchange", onPopState);
  }
  listeners.add(listener);
  return () => {
    listeners.delete(listener);
    if (listeners.size === 0) {
      removeEventListener("popstate", onPopState);
      removeEventListener("hashchange", onPopState);
    }
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
