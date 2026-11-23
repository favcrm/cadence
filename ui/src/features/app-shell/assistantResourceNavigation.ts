import { appViewRef, type AppViewRef } from "../canvas/appView";

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

/**
 * CAD-2471: the same resource as an app view for the canvas — a reference to
 * the installed screen, never its content. `null` when there is no link, no
 * screen tag, or an id the frame would refuse; the plain link keeps working.
 */
export function resourceAppView(appPath: string | null, installId: string, contextId: string, screenTag: string | null, ref: ResourceRef, appLabel: string | null = null): AppViewRef | null {
  const href = resourceHref(appPath, installId, contextId, ref);
  const section = RESOURCE_SECTIONS.get(ref.kind);
  if (href === null || section === undefined || screenTag === null) return null;
  return appViewRef({ installId, tag: screenTag, contextId, route: { section, view: "list", record_id: ref.id, segment_id: null }, title: ref.label, href, ...(appLabel ? { appLabel } : {}) });
}
