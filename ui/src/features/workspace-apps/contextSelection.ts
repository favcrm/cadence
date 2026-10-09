const key = (installId: string) => `cadence.workspace-app.context.${installId}`;

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

export function initialContext(installId: string, activeIds: string[]): string {
  const remembered = rememberedContext(installId);
  if (remembered !== null && (remembered === "" || activeIds.includes(remembered))) return remembered;
  const selected = activeIds.length === 1 ? activeIds[0] : "";
  rememberContext(installId, selected);
  return selected;
}

/** A private screen has no context picker, so a lone active context is the
 *  workspace's company and is always the selection (even over a remembered
 *  "no context"). Zero actives select nothing — never an invented context —
 *  and several keep the explicit-choice rules of {@link initialContext}. */
export function screenContext(installId: string, activeIds: string[]): string {
  if (activeIds.length === 1) {
    if (rememberedContext(installId) !== activeIds[0]) rememberContext(installId, activeIds[0]);
    return activeIds[0];
  }
  return initialContext(installId, activeIds);
}
