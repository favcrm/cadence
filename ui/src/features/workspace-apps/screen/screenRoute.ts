import type { ActionArgs, ScreenNavigation, ScreenRoute } from "./screenProtocol";
import { exact } from "./screenServices";
import { paramsFromQuery, routeParams, withParams, type RouteParams } from "./routeParams";
const id = (value: unknown): value is string => typeof value === "string" && /^[a-z0-9_-]{1,64}$/.test(value);
export interface RoutePatch { section?: string; view?: "list" | "new"; record_id?: string | null; segment_id?: string | null; params?: RouteParams; replace?: boolean }
export function parseRoutePatch(args: ActionArgs, nav: ScreenNavigation): RoutePatch | null {
  if (!exact(args, ["section", "view", "record_id", "segment_id", "params", "replace"]) ||
      "section" in args && !nav.sections.some(s => s.id === args.section) ||
      "view" in args && args.view !== "list" && args.view !== "new" ||
      ["record_id", "segment_id"].some(k => k in args && args[k] !== null && !id(args[k])) ||
      "params" in args && !routeParams(args.params) ||
      "replace" in args && typeof args.replace !== "boolean") return null;
  return args as RoutePatch;
}
export function routeFromHref(nav: ScreenNavigation | null | undefined, tag: string, href: string): ScreenRoute {
  const q = new URLSearchParams(href.split("?")[1] ?? "");
  const section = nav?.sections.find(s => s.id === q.get(nav.section_query))?.id ?? nav?.default_section ?? tag;
  return { section, view: q.get("appview") === "new" ? "new" : "list", record_id: id(q.get("record")) ? q.get("record") : null, segment_id: id(q.get("segment")) ? q.get("segment") : null, ...withParams(paramsFromQuery(q)) };
}
/** The route a frame is pushed: its owner's, when it has one (the canvas's app
 *  view), else the board URL's. A held route never consults the URL. */
export function frameRoute(nav: ScreenNavigation | null | undefined, tag: string, current: { route?: ScreenRoute; href: string }): ScreenRoute {
  const { route } = current;
  if (!route) return routeFromHref(nav, tag, current.href);
  return nav?.sections.some(s => s.id === route.section) ? route : { ...route, section: nav?.default_section ?? tag };
}
