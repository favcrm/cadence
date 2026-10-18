import { onPrivateSessionReset } from "../../lib/privateSession";

const key = (installId: string) => `cadence.workspace-app.context.${installId}`;

onPrivateSessionReset(() => {
  const prefix = "cadence.workspace-app.context.";
  try {
    const storage = window.sessionStorage;
    for (let index = storage.length - 1; index >= 0; index -= 1) {
      const saved = storage.key(index);
      if (!saved?.startsWith(prefix)) continue;
      storage.removeItem(saved);
      emitContext(saved.slice(prefix.length), null);
    }
  } catch {
    // Storage is optional; the private tree also discards its current frame.
  }
});

export function rememberedContext(installId: string): string | null {
  try { return window.sessionStorage.getItem(key(installId)); }
  catch { return null; }
}

/** Same-tab subscribers to one installation's context selection.
 *  `sessionStorage` emits no event in its own tab, so the shell
 *  cannot poll it: every write notifies listeners synchronously. */
type ContextListener = (installId: string, contextId: string | null) => void;
const listeners = new Set<ContextListener>();

export function subscribeContext(listener: ContextListener): () => void {
  listeners.add(listener);
  return () => {
    listeners.delete(listener);
  };
}

function emitContext(installId: string, contextId: string | null): void {
  for (const listener of [...listeners]) {
    try {
      listener(installId, contextId);
    } catch {
      /* One observer never breaks the write or the others. */
    }
  }
}

export function rememberContext(installId: string, contextId: string): void {
  try { window.sessionStorage.setItem(key(installId), contextId); }
  catch { /* Private mode can block storage; the in-memory selection still works. */ }
  emitContext(installId, contextId);
}

export function forgetContext(installId: string): void {
  try { window.sessionStorage.removeItem(key(installId)); }
  catch { /* Access loss still clears in-memory app data. */ }
  emitContext(installId, null);
}

/** Resolve a remembered selection against the live context list. Pure:
 *  no storage access — the scope owner reads/persists/announces around
 *  this call. Fail-closed: a remembered value that no longer names an
 *  active context resolves to THAT stale value (the caller's binding
 *  check then refuses "not active"), never silently broadened — until
 *  the operator makes an explicit pick. Fresh no-selection (null) and
 *  explicit "" stay as before. */
export function initialContext(remembered: string | null, activeIds: string[]): string {
  if (remembered !== null) return remembered;
  return activeIds.length === 1 ? activeIds[0] : "";
}

/** A private screen has no context picker, so a lone active context is
 *  the workspace's company and is the selection over a fresh or
 *  explicitly-empty remembered choice. A remembered NONEMPTY id is
 *  returned verbatim — even when stale (fail-closed refusal, never an
 *  auto-retarget to a different live context; the operator recovers by
 *  picking). Pure like {@link initialContext}. */
export function screenContext(remembered: string | null, activeIds: string[]): string {
  if (remembered === null) return activeIds.length === 1 ? activeIds[0] : "";
  if (remembered === "") return activeIds.length === 1 ? activeIds[0] : "";
  return remembered;
}
