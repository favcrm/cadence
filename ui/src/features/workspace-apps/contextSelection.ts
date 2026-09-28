const key = (installId: string) => `cadence.workspace-app.context.${installId}`;

export function rememberedContext(installId: string): string | null {
  try { return window.sessionStorage.getItem(key(installId)); }
  catch { return null; }
}

export function rememberContext(installId: string, contextId: string): void {
  try { window.sessionStorage.setItem(key(installId), contextId); }
  catch { /* Private mode can block storage; the in-memory selection still works. */ }
}

export function forgetContext(installId: string): void {
  try { window.sessionStorage.removeItem(key(installId)); }
  catch { /* Access loss still clears in-memory app data. */ }
}

export function initialContext(installId: string, activeIds: string[]): string {
  const remembered = rememberedContext(installId);
  if (remembered !== null && (remembered === "" || activeIds.includes(remembered))) return remembered;
  const selected = activeIds.length === 1 ? activeIds[0] : "";
  rememberContext(installId, selected);
  return selected;
}
