/**
 * CAD-2471: an app view is a canvas tab that points at one screen of an
 * installed app. It is a reference only — never content: the tab body mounts
 * the screen through the ordinary screen outlet, so the viewer's own session
 * decides what it may see.
 */
import type { ScreenRoute } from "../workspace-apps/screen/screenProtocol";
import type { RoutePatch } from "../workspace-apps/screen/screenRoute";

export interface AppViewRef {
  installId: string;
  /** The installed screen's tag. */
  tag: string;
  contextId: string;
  /** Where the screen opens; the tab keeps it local (it never reaches the board URL). */
  route: ScreenRoute;
  title: string;
  /** The full-page link (`/apps/<key>?…`) that "Open in <App>" navigates to. */
  href: string;
}

// The frame's own id and tag grammars (screenRoute.ts, screens.json): a ref
// the frame would refuse is not a tab.
const FRAME_ID = /^[a-z0-9_-]{1,64}$/;
const SCREEN_TAG = /^[a-z0-9][a-z0-9-]{0,31}$/;
const HOST_ID = /^[A-Za-z0-9_-]{1,128}$/;
const APP_HREF = /^\/apps\/[A-Za-z0-9][A-Za-z0-9._-]{0,127}(\?[A-Za-z0-9=&_%.-]{0,512})?$/;

const frameId = (value: unknown): value is string => typeof value === "string" && FRAME_ID.test(value);
const nullableFrameId = (value: unknown): value is string | null => value === null || frameId(value);

/** A validated reference, or null: a malformed one opens no tab. */
export function appViewRef(input: unknown): AppViewRef | null {
  if (typeof input !== "object" || input === null || Array.isArray(input)) return null;
  const { installId, tag, contextId, route, title, href } = input as Record<string, unknown>;
  if (typeof installId !== "string" || !HOST_ID.test(installId)) return null;
  if (typeof tag !== "string" || !SCREEN_TAG.test(tag)) return null;
  if (typeof contextId !== "string" || (contextId !== "" && !HOST_ID.test(contextId))) return null;
  if (typeof title !== "string" || title.trim() === "" || title.length > 200) return null;
  if (typeof href !== "string" || !APP_HREF.test(href) || href.includes("..")) return null;
  if (typeof route !== "object" || route === null || Array.isArray(route)) return null;
  const { section, view, record_id, segment_id } = route as Record<string, unknown>;
  if (!frameId(section) || (view !== "list" && view !== "new") || !nullableFrameId(record_id) || !nullableFrameId(segment_id)) return null;
  return { installId, tag, contextId, route: { section, view, record_id, segment_id }, title: title.trim(), href };
}

/** One tab per install, screen and record; opening it again focuses it. The
 *  `app:` prefix keeps it clear of artifact ids (`art-…`). */
export function appViewId(ref: AppViewRef): string {
  return `app:${[ref.installId, ref.contextId, ref.tag, ref.route.section, ref.route.record_id ?? ""].join(":")}`;
}

/** A route patch from inside the frame, applied to the tab's own route.
 *  The full-page app writes the same patch to the board URL; here it goes nowhere else. */
export function applyRoutePatch(route: ScreenRoute, patch: RoutePatch): ScreenRoute {
  return {
    section: patch.section ?? route.section,
    view: patch.view ?? route.view,
    record_id: patch.record_id === undefined ? route.record_id : patch.record_id,
    segment_id: patch.segment_id === undefined ? route.segment_id : patch.segment_id,
  };
}
