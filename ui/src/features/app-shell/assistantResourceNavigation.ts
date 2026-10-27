/** A resource reference emitted by app-assistant operations. */
export interface ResourceRef { kind: string; id: string; label: string }

// The host owns app route keys; generic assistant rendering must not name them.
const RESOURCE_SECTIONS = new Map<string, string>([
  ["customer", "customers"], ["customers", "customers"],
  ["segment", "segments"], ["segments", "segments"],
  ["campaign", "campaigns"], ["campaigns", "campaigns"],
]);

/**
 * CAD-1438: deep links emit the canonical `/apps/<key>` route only. `appPath`
 * is the verified installation's canonical path (`installationAppHref`),
 * already proven unique by the caller — never a caller-built route. `null`
 * (ambiguous, removed, unsafe) means no link. `install` pins the immutable
 * installation id so a replacement install at the same key can never open
 * this context and record silently.
 */
export function resourceHref(appPath: string | null, installId: string, contextId: string, ref: ResourceRef): string | null {
  const section = RESOURCE_SECTIONS.get(ref.kind);
  if (!section || appPath === null || !/^[A-Za-z0-9_-]{1,128}$/.test(ref.id) ||
      !/^[A-Za-z0-9_-]{1,128}$/.test(installId) || !/^[A-Za-z0-9_-]{1,128}$/.test(contextId)) return null;
  const query = new URLSearchParams({ install: installId, ctx: contextId, crm: section, record: ref.id });
  return `${appPath}?${query.toString()}`;
}
