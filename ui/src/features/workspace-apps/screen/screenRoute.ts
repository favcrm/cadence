import type { ActionArgs, ScreenNavigation, ScreenRoute } from "./screenProtocol";
import { exact } from "./screenServices";
const id = (value: unknown): value is string => typeof value === "string" && /^[a-z0-9_-]{1,64}$/.test(value);
export interface RoutePatch { section?: string; view?: "list" | "new"; record_id?: string | null; segment_id?: string | null; replace?: boolean }
export function parseRoutePatch(args: ActionArgs, nav: ScreenNavigation): RoutePatch | null {
  if (!exact(args, ["section", "view", "record_id", "segment_id", "replace"]) ||
      "section" in args && !nav.sections.some(s => s.id === args.section) ||
      "view" in args && args.view !== "list" && args.view !== "new" ||
      ["record_id", "segment_id"].some(k => k in args && args[k] !== null && !id(args[k])) ||
      "replace" in args && typeof args.replace !== "boolean") return null;
  return args as RoutePatch;
}
export function routeFromHref(nav: ScreenNavigation | null | undefined, tag: string, href: string): ScreenRoute {
  const q = new URLSearchParams(href.split("?")[1] ?? "");
  const section = nav?.sections.find(s => s.id === q.get(nav.section_query))?.id ?? nav?.default_section ?? tag;
  return { section, view: q.get("appview") === "new" ? "new" : "list", record_id: id(q.get("record")) ? q.get("record") : null, segment_id: id(q.get("segment")) ? q.get("segment") : null };
}
