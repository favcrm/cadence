/** A resource reference emitted by app-assistant operations. */
export interface ResourceRef { kind: string; id: string; label: string }

// The host owns app route keys; generic assistant rendering must not name them.
const RESOURCE_SECTIONS = new Map<string, string>([
  ["customer", "customers"], ["customers", "customers"],
  ["segment", "segments"], ["segments", "segments"],
  ["campaign", "campaigns"], ["campaigns", "campaigns"],
]);

export function resourceHref(installId: string, contextId: string, ref: ResourceRef): string | null {
  const section = RESOURCE_SECTIONS.get(ref.kind);
  if (!section || !/^[A-Za-z0-9_-]{1,128}$/.test(ref.id) ||
      !/^[A-Za-z0-9_-]{1,128}$/.test(installId) || !/^[A-Za-z0-9_-]{1,128}$/.test(contextId)) return null;
  const query = new URLSearchParams({ ctx: contextId, crm: section, record: ref.id });
  return `/app-installations/${encodeURIComponent(installId)}?${query.toString()}`;
}
